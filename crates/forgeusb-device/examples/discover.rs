//! Read-only diagnostic: never opens a block device for writing.
fn main() -> anyhow::Result<()> {
    for device in forgeusb_device::discover()? {
        println!(
            "{} {} bytes model={} transport={} eligible={}",
            device.path.display(),
            device.capacity,
            device.model,
            device.transport,
            device.eligible
        );
        for reason in device.reasons {
            println!("  refused: {reason}");
        }
        for partition in device.partitions {
            println!(
                "  {} {} bytes filesystem={:?} mounts={:?}",
                partition.path.display(),
                partition.capacity,
                partition.filesystem,
                partition.mounts
            );
        }
    }
    Ok(())
}
