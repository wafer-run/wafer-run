use std::sync::Arc;

use crate::interfaces::logger::{
    handler,
    service::{Field, LoggerService, RenderedFields},
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
/// default target, as an event of three string fields and no event message:
/// - `caller`: the sending block's registered name, `-` when there is none;
/// - `msg`: the block's message;
/// - `fields`: its structured fields rendered by [`RenderedFields`].
///
/// Every text the block wrote is a string field rather than the event
/// message because `tracing-subscriber`'s text formatter writes the message
/// bare but a string field quoted and escaped (`msg="hi caller=x/y"`), so a
/// message cannot read as a field of its own line; a JSON formatter writes
/// each as a JSON string either way.
pub struct TracingLogger;

/// The `caller` field of a [`TracingLogger`] event. A registered block name
/// always contains a `/`, so `-` cannot be mistaken for one.
fn caller_field(caller: Option<&str>) -> &str {
    caller.unwrap_or("-")
}

impl LoggerService for TracingLogger {
    fn debug(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        let fields = RenderedFields(fields).to_string();
        tracing::debug!(caller = caller_field(caller), msg, fields = fields.as_str());
    }

    fn info(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        let fields = RenderedFields(fields).to_string();
        tracing::info!(caller = caller_field(caller), msg, fields = fields.as_str());
    }

    fn warn(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        let fields = RenderedFields(fields).to_string();
        tracing::warn!(caller = caller_field(caller), msg, fields = fields.as_str());
    }

    fn error(&self, caller: Option<&str>, msg: &str, fields: &[Field]) {
        let fields = RenderedFields(fields).to_string();
        tracing::error!(caller = caller_field(caller), msg, fields = fields.as_str());
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

    /// Every event's fields, by name, as the subscriber received them. A
    /// field recorded through `record_debug` (not as a string, so a text
    /// formatter would write it bare) is keyed `debug:{name}`.
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
                .insert(format!("debug:{}", field.name()), format!("{value:?}"));
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

    /// Run `logger.info` from `acme/feature` with `message` and `fields`
    /// through the real handler into [`TracingLogger`]; the captured events.
    async fn log_from_feature(
        message: &str,
        fields: HashMap<String, serde_json::Value>,
    ) -> Vec<HashMap<String, String>> {
        let events = Events::default();
        let body = codec::encode(&LogRequest {
            message: message.into(),
            fields,
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
        let captured = events.lock().unwrap().clone();
        captured
    }

    /// A block's log line reaches `tracing` as one event that names the
    /// block, with its newlines escaped in the message and in the fields —
    /// so the fmt subscriber writes it as one line a reader can attribute.
    #[tokio::test]
    async fn a_block_log_line_is_one_attributed_event() {
        let events = log_from_feature(
            "ok\nERROR wafer_core: forged record",
            HashMap::from([("note".to_string(), serde_json::json!("a\nWARN forged"))]),
        )
        .await;

        assert_eq!(events.len(), 1, "{events:?}");
        let event = &events[0];
        assert_eq!(event["caller"], "acme/feature");
        assert_eq!(event["msg"], "ok\\nERROR wafer_core: forged record");
        assert_eq!(event["fields"], r#"note="a\nWARN forged""#);
        assert!(
            event.values().all(|v| !v.contains('\n')),
            "no field may carry a raw newline: {event:?}"
        );
    }

    /// Every text a block writes reaches `tracing` as a string field, never
    /// as the bare event message, so the text formatter quotes it: a message
    /// spelling `caller=…` stays inside `msg="…"`, and a field value with a
    /// space or `=` is quoted inside `fields`.
    #[tokio::test]
    async fn block_text_cannot_read_as_a_field_of_its_line() {
        let events = log_from_feature(
            "hi caller=wafer-run/admin",
            HashMap::from([(
                "note".to_string(),
                serde_json::json!("x caller=wafer-run/admin"),
            )]),
        )
        .await;

        assert_eq!(
            events,
            vec![HashMap::from([
                ("caller".to_string(), "acme/feature".to_string()),
                ("msg".to_string(), "hi caller=wafer-run/admin".to_string()),
                (
                    "fields".to_string(),
                    r#"note="x caller=wafer-run/admin""#.to_string()
                ),
            ])],
            "one event, all three recorded as strings, and no bare message"
        );
    }
}
