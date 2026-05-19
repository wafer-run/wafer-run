//! `Wafer::start_with_priority` must run `Init` on the named priority
//! blocks BEFORE `init_all_blocks` initialises the rest. Otherwise blocks
//! whose `Init` depends on shared infrastructure created by another block
//! (e.g. an admin block whose migrations create the `block_settings` table
//! that every other block writes to during its own `Init`) can lose the
//! `HashMap::keys()` ordering race and permanent-fail.

use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

use async_trait::async_trait;
use wafer_block::{
    core_types::{LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo,
};
use wafer_run::{StaticConfigSource, Wafer};

struct OrderRecordingBlock {
    name: &'static str,
    counter: Arc<AtomicUsize>,
    init_order: Arc<Mutex<Vec<(&'static str, usize)>>>,
}

#[async_trait]
impl Block for OrderRecordingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(self.name, "0.1.0", "test/iface@v1", "test")
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        event: LifecycleEvent,
    ) -> Result<(), WaferError> {
        if event.event_type == LifecycleType::Init {
            let n = self.counter.fetch_add(1, Ordering::SeqCst);
            self.init_order
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push((self.name, n));
        }
        Ok(())
    }

    fn bind(&self, _handle: Box<dyn std::any::Any + Send + Sync>) {}

    async fn handle(
        &self,
        _ctx: &dyn wafer_block::context::Context,
        _msg: Message,
        _input: InputStream,
    ) -> OutputStream {
        OutputStream::respond(Vec::new())
    }
}

#[tokio::test]
async fn start_with_priority_inits_named_blocks_first() {
    let counter = Arc::new(AtomicUsize::new(0));
    let init_order: Arc<Mutex<Vec<(&'static str, usize)>>> = Arc::new(Mutex::new(Vec::new()));

    let make = |name: &'static str| -> Arc<OrderRecordingBlock> {
        Arc::new(OrderRecordingBlock {
            name,
            counter: counter.clone(),
            init_order: init_order.clone(),
        })
    };

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    // Register in an order that would NOT happen to put the priority block
    // first under HashMap iteration; the priority list is the only thing
    // forcing the ordering.
    for name in ["test/alpha", "test/admin", "test/beta", "test/gamma"] {
        wafer.register_block(name, make(name)).expect("register");
    }

    let _arc = wafer
        .start_with_priority(&["test/admin"])
        .await
        .expect("start_with_priority");

    let order = init_order.lock().unwrap_or_else(|p| p.into_inner()).clone();
    assert_eq!(
        order.len(),
        4,
        "every block must have been initialised once"
    );
    assert_eq!(
        order[0].0, "test/admin",
        "priority block must be the first Init: order = {order:?}",
    );
    // Slot caching means each block's Init lifecycle fires exactly once even
    // though `start_with_priority` calls `init_block(admin)` and then
    // `init_all_blocks` also visits admin.
    let admin_inits = order
        .iter()
        .filter(|(name, _)| *name == "test/admin")
        .count();
    assert_eq!(
        admin_inits, 1,
        "admin Init must fire exactly once across both passes",
    );
}

#[tokio::test]
async fn start_with_priority_skips_unknown_names() {
    let counter = Arc::new(AtomicUsize::new(0));
    let init_order: Arc<Mutex<Vec<(&'static str, usize)>>> = Arc::new(Mutex::new(Vec::new()));

    let cfg_src: Arc<dyn wafer_run::ConfigSource> = Arc::new(StaticConfigSource::default());
    let mut wafer = Wafer::new(cfg_src).expect("Wafer::new");
    wafer
        .register_block(
            "test/only",
            Arc::new(OrderRecordingBlock {
                name: "test/only",
                counter: counter.clone(),
                init_order: init_order.clone(),
            }),
        )
        .expect("register");

    // Unknown name in priority list must not panic or short-circuit start.
    let _arc = wafer
        .start_with_priority(&["test/nonexistent", "test/only"])
        .await
        .expect("start_with_priority");

    assert_eq!(counter.load(Ordering::SeqCst), 1);
}
