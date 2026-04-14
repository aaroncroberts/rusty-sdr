#![forbid(unsafe_code)]

use tokio::task::JoinHandle;

/// Every DSP node in the signal graph implements this trait.
///
/// Blocks are started once, run until stopped, and clean up on drop.
/// The tokio JoinHandle lets callers await shutdown.
pub trait Block: Send + 'static {
    fn start(&mut self) -> JoinHandle<()>;
    fn stop(&self);
}
