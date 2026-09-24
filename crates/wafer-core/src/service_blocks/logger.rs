use std::sync::Arc;

use crate::interfaces::logger::{
    handler,
    service::{Field, LoggerService},
};

crate::service_block! {
    /// Unified logger block. Wraps any `LoggerService` implementation.
    block: pub LoggerBlock,
    name: "wafer-run/logger",
    version: "0.0.1",
    interface: "logger@v1",
    description: "Structured logging",
    category: Service,
    fields: { service: Arc<dyn LoggerService> },
    handle: |this, ctx, msg, body| handler::handle_message(this.service.as_ref(), ctx, &msg, &body),
}

/// [`LoggerService`] implementation that forwards every call to the `tracing`
/// crate at the matching level (`debug!`/`info!`/`warn!`/`error!`) on the
/// default target. Each event carries the record as fields: `caller` (the
/// sending block's registered name, `-` when there is none), the message,
/// and `fields`, the structured fields rendered as `key=value` pairs.
pub struct TracingLogger;

/// The `caller` field of a [`TracingLogger`] event. A registered block name
/// always contains a `/`, so `-` cannot be mistaken for one.
fn caller_field(caller: Option<&str>) -> &str {
    caller.unwrap_or("-")
}

impl LoggerService for TracingLogger {
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        tracing::debug!(caller = caller_field(caller), fields = %Rendered(fields), "{msg}");
    }

    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        tracing::info!(caller = caller_field(caller), fields = %Rendered(fields), "{msg}");
    }

    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        tracing::warn!(caller = caller_field(caller), fields = %Rendered(fields), "{msg}");
    }

    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        tracing::error!(caller = caller_field(caller), fields = %Rendered(fields), "{msg}");
    }
}

/// `Display` adapter rendering structured fields as space-separated
/// `key=value` pairs.
struct Rendered<'a>(&'a [Field]);

impl std::fmt::Display for Rendered<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for (i, field) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(" ")?;
            }
            write!(f, "{}={}", field.key, field.value)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use tracing::{
        field::{Field as EventField, Visit},
        span, Event, Metadata, Subscriber,
    };
    use wafer_block::{
        codec,
        common::ServiceOp,
        context::Context,
        streams::{input::InputStream, output::OutputStream},
        wire::logger::LogRequest,
        Message,
    };

    use super::TracingLogger;
    use crate::interfaces::logger::handler;

    /// Every event's fields, by name, as the subscriber received them.
    type Events = Arc<Mutex<Vec<HashMap<String, String>>>>;

    /// A subscriber that keeps each event's recorded fields.
    struct Capture(Events);

    struct Fields<'a>(&'a mut HashMap<String, String>);

    impl Visit for Fields<'_> {
        fn record_str(&mut self, field: &EventField, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
        fn record_debug(&mut self, field: &EventField, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}"));
        }
    }

    impl Subscriber for Capture {
        fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
            true
        }
        fn new_span(&self, _span: &span::Attributes<'_>) -> span::Id {
            span::Id::from_u64(1)
        }
        fn record(&self, _span: &span::Id, _values: &span::Record<'_>) {}
        fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}
        fn event(&self, event: &Event<'_>) {
            let mut fields = HashMap::new();
            event.record(&mut Fields(&mut fields));
            self.0.lock().unwrap().push(fields);
        }
        fn enter(&self, _span: &span::Id) {}
        fn exit(&self, _span: &span::Id) {}
    }

    /// The logger block's context for a call from `acme/feature`.
    struct FromFeature;

    #[wafer_block::wafer_async_trait]
    impl Context for FromFeature {
        async fn call_block(&self, _b: &str, _m: Message, _i: InputStream) -> OutputStream {
            unimplemented!("the logger handler makes no calls")
        }
        fn is_cancelled(&self) -> bool {
            false
        }
        fn config_get(&self, _key: &str) -> Option<&str> {
            None
        }
        fn clone_arc(&self) -> Arc<dyn Context> {
            unimplemented!("the logger handler keeps no context")
        }
        fn caller_id(&self) -> Option<&str> {
            Some("acme/feature")
        }
        // The logger authorizes nothing; deny, as `check_resource_access`'s
        // default does.
        fn resource_access_admitted(
            &self,
            _resource: &str,
            _resource_type: wafer_block::types::ResourceType,
            _access: wafer_block::types::ResourceAccess,
        ) -> bool {
            false
        }
    }

    /// A block's log line reaches `tracing` as one event that names the
    /// block, with its newlines escaped in the message and in the fields —
    /// so the fmt subscriber writes it as one line a reader can attribute.
    #[tokio::test]
    async fn a_block_log_line_is_one_attributed_event() {
        let events = Events::default();
        let body = codec::encode(&LogRequest {
            message: "ok\nERROR wafer_core: forged record".into(),
            fields: HashMap::from([("note".to_string(), serde_json::json!("a\nWARN forged"))]),
        })
        .unwrap();

        let out = tracing::subscriber::with_default(Capture(events.clone()), || {
            handler::handle_message(
                &TracingLogger,
                &FromFeature,
                &Message::new(ServiceOp::LOGGER_INFO),
                &body,
            )
        });
        out.collect_buffered().await.expect("logger.info responds");

        let events = events.lock().unwrap().clone();
        assert_eq!(events.len(), 1, "{events:?}");
        let event = &events[0];
        assert_eq!(event["caller"], "acme/feature");
        assert_eq!(event["message"], "ok\\nERROR wafer_core: forged record");
        assert_eq!(event["fields"], "note=a\\nWARN forged");
        assert!(
            event.values().all(|v| !v.contains('\n')),
            "no field may carry a raw newline: {event:?}"
        );
    }
}
