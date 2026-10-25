use clap::Parser;
use procfs::prelude::*;
use procutils_common::{MAX_TERM_WIDTH, man::ManContent};
use std::process::ExitCode;

pub const MAN: ManContent = ManContent {
    description: Some(include_str!("../man/description.man")),
    extra_sections: &[
        (
            "FIELD DESCRIPTIONS",
            include_str!("../man/field_descriptions.man"),
        ),
        ("EXAMPLES", include_str!("../man/examples.man")),
        ("NOTES", include_str!("../man/notes.man")),
        ("DIVERGENCES", include_str!("../man/divergences.man")),
        ("SEE ALSO", include_str!("../man/see_also.man")),
    ],
};

// Procps-style fixed column widths: a left-aligned label region of 8
// chars, then each value right-aligned in 12. Matches procps-ng `free`
// byte-for-byte.
const LABEL_W: usize = 8;
const VAL_W: usize = 12;

const FORMATTING: &str = "Formatting";
const DISPLAY: &str = "Display";
const LAYOUT: &str = "Layout";
const REPEAT: &str = "Repeat";

/// Display amount of free and used memory in the system.
#[derive(Parser)]
#[command(name = "free", version, about, disable_help_flag = true, max_term_width = MAX_TERM_WIDTH)]
pub struct Args {
    /// Show human-readable output.
    ///
    /// Scale each value independently to fit three numeric columns, using
    /// binary prefixes (Ki, Mi, Gi). Use `--si` for decimal prefixes (K, M, G).
    #[arg(short = 'h', long, help_heading = FORMATTING)]
    human: bool,

    /// Use powers of 1000 not 1024.
    #[arg(long, help_heading = FORMATTING)]
    si: bool,

    /// Show output in bytes.
    #[arg(short, long, help_heading = FORMATTING)]
    bytes: bool,

    /// Show output in kilobytes (1000¹).
    #[arg(long, help_heading = FORMATTING)]
    kilo: bool,

    /// Show output in megabytes (1000²).
    #[arg(long, help_heading = FORMATTING)]
    mega: bool,

    /// Show output in gigabytes (1000³).
    #[arg(long, help_heading = FORMATTING)]
    giga: bool,

    /// Show output in terabytes (1000⁴).
    #[arg(long, help_heading = FORMATTING)]
    tera: bool,

    /// Show output in petabytes (1000⁵).
    #[arg(long, help_heading = FORMATTING)]
    peta: bool,

    /// Show output in kibibytes (1024¹).
    #[arg(short = 'k', long, help_heading = FORMATTING)]
    kibi: bool,

    /// Show output in mebibytes (1024²).
    #[arg(short = 'm', long, help_heading = FORMATTING)]
    mebi: bool,

    /// Show output in gibibytes (1024³).
    #[arg(short = 'g', long, help_heading = FORMATTING)]
    gibi: bool,

    /// Show output in tebibytes (1024⁴).
    #[arg(long, help_heading = FORMATTING)]
    tebi: bool,

    /// Show output in pebibytes (1024⁵).
    #[arg(long, help_heading = FORMATTING)]
    pebi: bool,

    /// Show detailed low and high memory statistics.
    #[arg(short, long, help_heading = DISPLAY)]
    lohi: bool,

    /// Show output on a single line.
    #[arg(short = 'L', long, help_heading = LAYOUT)]
    line: bool,

    /// Show total for RAM + swap.
    #[arg(short, long, help_heading = DISPLAY)]
    total: bool,

    /// Show committed memory and commit limit.
    #[arg(short = 'v', long, help_heading = DISPLAY)]
    committed: bool,

    /// Repeat printing every N seconds.
    #[arg(short = 's', long, help_heading = REPEAT)]
    seconds: Option<f64>,

    /// Repeat printing N times, then exit.
    #[arg(short, long, help_heading = REPEAT)]
    count: Option<u64>,

    /// Wide output (separate buffers and cache).
    #[arg(short, long, help_heading = LAYOUT)]
    wide: bool,

    /// Display this help and exit.
    #[arg(long, action = clap::ArgAction::Help)]
    help: Option<bool>,
}

struct Unit {
    divisor: f64,
}

impl Args {
    /// Returns the unit to use for display. The divisor converts bytes
    /// to the target unit.
    fn unit(&self) -> Unit {
        let ki = 1024.0_f64;
        let mi = ki * 1024.0;
        let gi = mi * 1024.0;
        let ti = gi * 1024.0;
        let pi = ti * 1024.0;

        if self.bytes {
            return Unit { divisor: 1.0 };
        }
        // SI units (powers of 1000)
        if self.kilo {
            return Unit { divisor: 1_000.0 };
        }
        if self.mega {
            return Unit {
                divisor: 1_000_000.0,
            };
        }
        if self.giga {
            return Unit {
                divisor: 1_000_000_000.0,
            };
        }
        if self.tera {
            return Unit {
                divisor: 1_000_000_000_000.0,
            };
        }
        if self.peta {
            return Unit {
                divisor: 1_000_000_000_000_000.0,
            };
        }
        // Binary units (powers of 1024)
        if self.mebi {
            return Unit { divisor: mi };
        }
        if self.gibi {
            return Unit { divisor: gi };
        }
        if self.tebi {
            return Unit { divisor: ti };
        }
        if self.pebi {
            return Unit { divisor: pi };
        }
        // Default: kibibytes
        Unit { divisor: ki }
    }
}

struct MemData {
    mem_total: u64,
    mem_free: u64,
    mem_available: Option<u64>,
    buffers: u64,
    cached: u64,
    shared: u64,
    swap_total: u64,
    swap_free: u64,
    low_total: u64,
    low_free: u64,
    high_total: u64,
    high_free: u64,
    commit_limit: u64,
    committed_as: u64,
}

impl MemData {
    fn from_meminfo(m: &procfs::Meminfo) -> Self {
        // cache = Cached + SReclaimable; matches procps and the free(1) man
        // page description of the "cache" column.
        let s_reclaimable = m.s_reclaimable.unwrap_or(0);
        let cached = m.cached + s_reclaimable;
        let shared = m.shmem.unwrap_or(0);

        Self {
            mem_total: m.mem_total,
            mem_free: m.mem_free,
            mem_available: m.mem_available,
            buffers: m.buffers,
            cached,
            shared,
            swap_total: m.swap_total,
            swap_free: m.swap_free,
            low_total: m.low_total.unwrap_or(0),
            low_free: m.low_free.unwrap_or(0),
            high_total: m.high_total.unwrap_or(0),
            high_free: m.high_free.unwrap_or(0),
            commit_limit: m.commit_limit.unwrap_or(0),
            committed_as: m.committed_as,
        }
    }

    fn mem_used(&self) -> u64 {
        match self.mem_available {
            Some(avail) => self.mem_total.saturating_sub(avail),
            // Pre-3.14 kernels: MemAvailable absent, fall back to the
            // legacy formula.
            None => self
                .mem_total
                .saturating_sub(self.mem_free)
                .saturating_sub(self.buffers)
                .saturating_sub(self.cached),
        }
    }

    fn mem_available(&self) -> u64 {
        self.mem_available.unwrap_or(self.mem_free)
    }

    fn buff_cache(&self) -> u64 {
        self.buffers + self.cached
    }

    fn swap_used(&self) -> u64 {
        self.swap_total.saturating_sub(self.swap_free)
    }
}

pub fn run(args: Args) -> ExitCode {
    if let Some(0) = args.count {
        eprintln!("free: count must be at least 1");
        return ExitCode::from(2);
    }
    let iterations = match (args.seconds, args.count) {
        (None, None) => 1,
        (Some(_), None) => u64::MAX,
        (None, Some(c)) => c,
        (Some(_), Some(c)) => c,
    };
    let interval = args.seconds.map(std::time::Duration::from_secs_f64);

    for i in 0..iterations {
        if i > 0 {
            if let Some(d) = interval {
                std::thread::sleep(d);
            }
            println!();
        }

        let meminfo = match procfs::Meminfo::current() {
            Ok(m) => m,
            Err(e) => {
                eprintln!("free: {e}");
                return ExitCode::FAILURE;
            }
        };
        let data = MemData::from_meminfo(&meminfo);

        if args.line {
            print_line(&args, &data);
        } else {
            print_table(&args, &data);
        }
    }

    ExitCode::SUCCESS
}

fn format_value(bytes: u64, unit: &Unit, human: bool, si: bool) -> String {
    if human {
        human_readable(bytes, si)
    } else {
        let converted = bytes as f64 / unit.divisor;
        format!("{}", converted as u64)
    }
}

fn human_readable(bytes: u64, si: bool) -> String {
    if bytes < 1000 {
        return format!("{bytes}B");
    }

    let (base, suffixes): (u64, &[&str]) = if si {
        (1000, &["K", "M", "G", "T", "P"])
    } else {
        (1024, &["Ki", "Mi", "Gi", "Ti", "Pi"])
    };
    let mut divisor = 1;
    for (index, suffix) in suffixes.iter().enumerate() {
        divisor *= base;
        // procps chooses the first representation fitting three numeric
        // columns, trying one decimal place before a truncated integer.
        let scaled = bytes as f64 / divisor as f64;
        let value = scaled as f32;
        if (f64::from(value) * 10.0).round_ties_even() < 100.0 {
            return format!("{value:.1}{suffix}");
        }
        let whole = scaled as u64;
        if whole < 1000 || index == suffixes.len() - 1 {
            return format!("{whole}{suffix}");
        }
    }
    unreachable!("the final unit always produces a representation")
}

fn print_line(args: &Args, data: &MemData) {
    let unit = args.unit();
    let fmt = |v: u64| format_value(v, &unit, args.human, args.si);

    // Two spaces between the CachUse value and "MemUse" mirror procps's
    // hard-coded format string.
    println!(
        "SwapUse {:>11} CachUse {:>11}  MemUse {:>11} MemFree {:>11}",
        fmt(data.swap_used()),
        fmt(data.buff_cache()),
        fmt(data.mem_used()),
        fmt(data.mem_free),
    );
}

fn print_header(args: &Args) {
    let cols: &[&str] = if args.wide {
        &[
            "total",
            "used",
            "free",
            "shared",
            "buffers",
            "cache",
            "available",
        ]
    } else {
        &["total", "used", "free", "shared", "buff/cache", "available"]
    };
    let mut out = format!("{:<LABEL_W$}", "");
    for c in cols {
        out.push_str(&format!("{c:>VAL_W$}"));
    }
    println!("{out}");
}

fn print_row(label: &str, values: &[String]) {
    let mut out = format!("{label:<LABEL_W$}");
    for v in values {
        out.push_str(&format!("{v:>VAL_W$}"));
    }
    println!("{out}");
}

fn print_table(args: &Args, data: &MemData) {
    let unit = args.unit();
    let fmt = |v: u64| format_value(v, &unit, args.human, args.si);

    print_header(args);

    let mem_values: Vec<String> = if args.wide {
        vec![
            fmt(data.mem_total),
            fmt(data.mem_used()),
            fmt(data.mem_free),
            fmt(data.shared),
            fmt(data.buffers),
            fmt(data.cached),
            fmt(data.mem_available()),
        ]
    } else {
        vec![
            fmt(data.mem_total),
            fmt(data.mem_used()),
            fmt(data.mem_free),
            fmt(data.shared),
            fmt(data.buff_cache()),
            fmt(data.mem_available()),
        ]
    };
    print_row("Mem:", &mem_values);

    if args.lohi {
        // CONFIG_HIGHMEM is rare on modern kernels; when /proc/meminfo
        // omits High*, treat all memory as low and emit a zeroed High
        // row to match procps.
        let (low_total, low_free, high_total, high_free) = if data.high_total > 0 {
            (
                data.low_total,
                data.low_free,
                data.high_total,
                data.high_free,
            )
        } else {
            (data.mem_total, data.mem_free, 0, 0)
        };
        print_row(
            "Low:",
            &[
                fmt(low_total),
                fmt(low_total.saturating_sub(low_free)),
                fmt(low_free),
            ],
        );
        print_row(
            "High:",
            &[
                fmt(high_total),
                fmt(high_total.saturating_sub(high_free)),
                fmt(high_free),
            ],
        );
    }

    print_row(
        "Swap:",
        &[
            fmt(data.swap_total),
            fmt(data.swap_used()),
            fmt(data.swap_free),
        ],
    );

    if args.total {
        print_row(
            "Total:",
            &[
                fmt(data.mem_total + data.swap_total),
                fmt(data.mem_used() + data.swap_used()),
                fmt(data.mem_free + data.swap_free),
            ],
        );
    }

    if args.committed {
        print_row(
            "Comm:",
            &[
                fmt(data.commit_limit),
                fmt(data.committed_as),
                fmt(data.commit_limit.saturating_sub(data.committed_as)),
            ],
        );
    }
}

#[cfg(test)]
mod tests {
    use super::human_readable;

    #[test]
    fn human_units_choose_a_three_column_representation_at_every_scale() {
        for (si, base, suffixes) in [
            (false, 1024_u64, ["Ki", "Mi", "Gi", "Ti", "Pi"]),
            (true, 1000_u64, ["K", "M", "G", "T", "P"]),
        ] {
            assert_eq!(human_readable(0, si), "0B");
            assert_eq!(human_readable(999, si), "999B");
            let mut unit = 1;
            for (index, suffix) in suffixes.iter().enumerate() {
                unit *= base;
                for (value, number) in [(1, "1.0"), (9, "9.0"), (10, "10"), (999, "999")] {
                    assert_eq!(
                        human_readable(value * unit, si),
                        format!("{number}{suffix}"),
                        "scale={index}, value={value}, si={si}",
                    );
                }
                if let Some(next) = suffixes.get(index + 1) {
                    assert_eq!(human_readable(1000 * unit, si), format!("1.0{next}"));
                }
            }
        }
        assert_eq!(human_readable(8704, false), "8.5Ki");
        assert_eq!(human_readable(10189, false), "9Ki");
        assert_eq!(human_readable(u64::MAX, false), "16384Pi");
    }
}
