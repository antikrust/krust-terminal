//! Can this machine actually run that model?
//!
//! The check is deliberately a *warning*, never a veto. Estimates here are
//! rules of thumb — quantisation varies per model, context length moves the
//! figure, and we cannot know what else the user has open. Blocking a download
//! on an estimate that is sometimes wrong would be worse than letting someone
//! make an informed mess.

use std::fmt;

/// Bytes per parameter at Ollama's default quantisation (roughly Q4_K_M).
///
/// A 7B model at Q4_K_M lands around 4.1 GB on disk; 0.6 bytes/param puts the
/// estimate slightly above that, which is the direction we want to err in
/// since weights are not the only thing resident at inference time.
const BYTES_PER_PARAM_Q4: f64 = 0.6;

/// Fixed overhead for the runtime, KV cache at a modest context, and the
/// rest of the process. Grows with model size in practice; this is a floor.
const RUNTIME_OVERHEAD_BYTES: u64 = 1_200_000_000;

/// Fraction of total RAM we treat as available to a model. The rest is the OS,
/// the browser, the editor, and this terminal. On unified-memory Macs the GPU
/// draws from the same pool, so there is no separate VRAM budget to consult.
const USABLE_MEMORY_FRACTION: f64 = 0.75;

/// A parameter count as advertised in the library listing: `8b`, `1.5b`, `70b`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ParameterSize {
    pub billions: f64,
}

impl ParameterSize {
    /// Parses a size tag as the library writes it.
    ///
    /// Handles the four forms seen on ollama.com: plain (`8b`, `1.5b`),
    /// millions (`270m`), mixture-of-experts (`8x7b`), and Gemma's
    /// effective-parameter notation (`e4b`).
    pub fn parse(tag: &str) -> Option<Self> {
        let tag = tag.trim().to_ascii_lowercase();

        let (number, scale) = if let Some(rest) = tag.strip_suffix('b') {
            (rest, 1.0)
        } else if let Some(rest) = tag.strip_suffix('m') {
            (rest, 0.001)
        } else {
            return None;
        };

        // `e4b` — effective parameters, used by Gemma 3n. The leading marker
        // carries no magnitude, so drop it.
        let number = number.strip_prefix('e').unwrap_or(number);

        // `8x7b` — mixture of experts. Every expert's weights must be
        // resident even though only some are active per token, so for a
        // memory estimate the total is what matters.
        let billions: f64 = if let Some((experts, per_expert)) = number.split_once('x') {
            let experts: f64 = experts.parse().ok()?;
            let per_expert: f64 = per_expert.parse().ok()?;
            experts * per_expert
        } else {
            number.parse().ok()?
        };

        (billions > 0.0 && billions.is_finite()).then_some(Self {
            billions: billions * scale,
        })
    }

    /// Estimated resident memory needed to run this model.
    pub fn estimated_bytes(self) -> u64 {
        let weights = self.billions * 1_000_000_000.0 * BYTES_PER_PARAM_Q4;
        weights as u64 + RUNTIME_OVERHEAD_BYTES
    }
}

impl fmt::Display for ParameterSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.billions < 1.0 {
            write!(f, "{:.0}M", self.billions * 1000.0)
        } else if self.billions.fract() == 0.0 {
            write!(f, "{:.0}B", self.billions)
        } else {
            write!(f, "{:.1}B", self.billions)
        }
    }
}

/// How well a model is expected to fit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Fits inside the usable-memory budget with room to spare.
    Comfortable,
    /// Fits in total RAM but eats into what everything else needs. Expect
    /// swapping and slow generation.
    Tight,
    /// Larger than the machine's entire RAM. It will thrash, if it runs.
    Exceeds,
}

impl Fit {
    /// Whether the UI should draw attention to this.
    pub fn is_warning(self) -> bool {
        matches!(self, Fit::Tight | Fit::Exceeds)
    }
}

/// The verdict for one model on this machine.
#[derive(Debug, Clone)]
pub struct MemoryVerdict {
    pub fit: Fit,
    pub required_bytes: u64,
    pub message: String,
}

/// Assesses `size` against a machine with `total_bytes` of RAM.
pub fn assess(size: ParameterSize, total_bytes: u64) -> MemoryVerdict {
    let required = size.estimated_bytes();
    let usable = (total_bytes as f64 * USABLE_MEMORY_FRACTION) as u64;

    let fit = if required > total_bytes {
        Fit::Exceeds
    } else if required > usable {
        Fit::Tight
    } else {
        Fit::Comfortable
    };

    let required_gb = format_gb(required);
    let total_gb = format_gb(total_bytes);
    let message = match fit {
        Fit::Comfortable => {
            format!("Needs about {required_gb}. You have {total_gb} — should be fine.")
        }
        Fit::Tight => format!(
            "Needs about {required_gb} of your {total_gb}. It will run, but expect \
             swapping and slow replies with anything else open."
        ),
        Fit::Exceeds => format!(
            "Needs about {required_gb}, more than this machine's {total_gb}. \
             Expect it to thrash rather than run."
        ),
    };

    MemoryVerdict {
        fit,
        required_bytes: required,
        message,
    }
}

pub fn format_gb(bytes: u64) -> String {
    let gb = bytes as f64 / 1_073_741_824.0;
    if gb < 10.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{gb:.0} GB")
    }
}

/// Total physical memory, or `None` if it can't be determined — in which case
/// callers should skip the check rather than guess.
pub fn total_system_memory_bytes() -> Option<u64> {
    #[cfg(target_os = "macos")]
    {
        // Unified memory: the GPU draws from this same pool, so there is no
        // separate VRAM figure worth consulting.
        read_sysctl_u64("hw.memsize")
    }
    #[cfg(target_os = "linux")]
    {
        let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
        let line = meminfo.lines().find(|l| l.starts_with("MemTotal:"))?;
        let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
        Some(kb * 1024)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn read_sysctl_u64(name: &str) -> Option<u64> {
    let output = std::process::Command::new("sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    String::from_utf8(output.stdout).ok()?.trim().parse().ok()
}

#[cfg(test)]
#[path = "capability_tests.rs"]
mod tests;
