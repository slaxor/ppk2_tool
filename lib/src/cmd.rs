//! Serial command opcodes understood by the PPK2 firmware.

/// Measurement mode: source meter feeds the DUT, ampere meter sits in series.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PowerMode {
    Source,
    Ampere,
}

/// A command as the firmware understands it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    GetMetadata,
    SetPowerMode(PowerMode),
    RegulatorSet { millivolts: u16 },
    DeviceRunning(bool),
    AverageStart,
    AverageStop,
    SpikeFiltering(bool),
    Reset,
}

impl Command {
    /// Wire encoding: one opcode byte plus big-endian arguments.
    pub fn to_bytes(self) -> Vec<u8> {
        match self {
            Command::GetMetadata => vec![0x19],
            Command::SetPowerMode(PowerMode::Source) => vec![0x11, 0x01],
            Command::SetPowerMode(PowerMode::Ampere) => vec![0x11, 0x02],
            Command::RegulatorSet { millivolts } => {
                let [hi, lo] = millivolts.to_be_bytes();
                vec![0x0d, hi, lo]
            }
            Command::DeviceRunning(on) => vec![0x0c, on as u8],
            Command::AverageStart => vec![0x06],
            Command::AverageStop => vec![0x07],
            Command::SpikeFiltering(true) => vec![0x15],
            Command::SpikeFiltering(false) => vec![0x16],
            Command::Reset => vec![0x20],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_like_the_reference_implementation() {
        let cases = [
            (Command::GetMetadata, vec![0x19]),
            (Command::SetPowerMode(PowerMode::Source), vec![0x11, 0x01]),
            (Command::SetPowerMode(PowerMode::Ampere), vec![0x11, 0x02]),
            (
                Command::RegulatorSet { millivolts: 3300 },
                vec![0x0d, 0x0c, 0xe4],
            ),
            (Command::DeviceRunning(true), vec![0x0c, 0x01]),
            (Command::DeviceRunning(false), vec![0x0c, 0x00]),
            (Command::AverageStart, vec![0x06]),
            (Command::AverageStop, vec![0x07]),
            (Command::Reset, vec![0x20]),
        ];
        for (cmd, want) in cases {
            assert_eq!(cmd.to_bytes(), want, "{cmd:?}");
        }
    }
}
