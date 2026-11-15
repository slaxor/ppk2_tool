use std::io::Write;
use std::time::{Duration, Instant};

use ppk2_tool::{PowerMode, Ppk2, find_ports};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let path = match args.next().filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => find_ports()?
            .into_iter()
            .next()
            .ok_or("no PPK2 found; pass the port path")?,
    };
    let secs: u64 = args.next().as_deref().unwrap_or("2").parse()?;

    let mut dev = Ppk2::open(&path, PowerMode::Source)?;
    eprintln!("{:#?}", dev.metadata);
    dev.set_source_voltage(3300)?;
    dev.power(true)?;
    dev.start_measuring()?;

    let mut out = std::io::stdout().lock();
    let mut samples = Vec::new();
    let until = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < until {
        samples.clear();
        dev.read_microamps(&mut samples)?;
        for ua in &samples {
            writeln!(out, "{ua:.3}")?;
        }
    }
    dev.stop_measuring()?;
    dev.power(false)?;
    Ok(())
}
