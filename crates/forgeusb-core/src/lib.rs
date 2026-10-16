//! Shared operation state. No device I/O or privilege transitions.
use serde::{Deserialize, Serialize};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Default, Debug)]
pub struct Cancellation(Arc<AtomicBool>);
impl Cancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
    pub fn check(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.is_cancelled(),
            "operation cancelled; target may be incomplete"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Inspecting,
    Revalidating,
    Writing,
    Flushing,
    Verifying,
    Complete,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub stage: Stage,
    pub completed: u64,
    pub total: u64,
    pub message: String,
}
pub fn progress(stage: Stage, completed: u64, total: u64, message: impl Into<String>) -> Progress {
    Progress {
        stage,
        completed,
        total,
        message: message.into(),
    }
}
pub fn capacity_check(image: u64, capacity: u64) -> anyhow::Result<()> {
    anyhow::ensure!(image > 0, "empty source image");
    anyhow::ensure!(
        image <= capacity,
        "image needs {image} bytes, target has {capacity} bytes"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounds() {
        assert!(capacity_check(1, 1).is_ok());
        assert!(capacity_check(0, 1).is_err());
        assert!(capacity_check(u64::MAX, 1).is_err());
    }
    #[test]
    fn cancellation() {
        let c = Cancellation::default();
        let d = c.clone();
        c.cancel();
        assert!(d.check().is_err());
    }
}
