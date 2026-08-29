use fearless_simd::Level;

/// What vector width and instruction sets this process actually got, for the boot log.
///
/// Every vectorised path in the server is written against `fearless_simd` and dispatched at
/// runtime on the detected [`Level`], which means the same binary runs 16, 32 or 64 bytes at a
/// time depending on the host and nothing in the build says which. Printing it once at boot is
/// the whole point of this type.
pub struct BasisSimdCapabilities;

impl BasisSimdCapabilities {
    /// Bytes processed per vector operation on this host at the dispatched level.
    pub fn vector_byte_width() -> usize {
        let level = Level::new();
        if level.is_fallback() {
            return 0;
        }
        Self::width_for(level)
    }

    fn width_for(level: Level) -> usize {
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            match level {
                Level::Avx512(_) => 64,
                Level::Avx2(_) => 32,
                Level::Sse4_2(_) | Level::Sse2(_) => 16,
                _ => 0,
            }
        }
        #[cfg(target_arch = "aarch64")]
        {
            match level {
                Level::Neon(_) => 16,
                _ => 0,
            }
        }
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
        {
            let _ = level;
            0
        }
    }

    /// False means every vector path is running as a scalar loop and is a red flag.
    pub fn hardware_accelerated() -> bool {
        !Level::new().is_fallback()
    }

    /// One line for the boot log: the width actually in force, then the instruction sets behind it.
    pub fn describe() -> String {
        let level = Level::new();
        let width = Self::width_for(level);
        let mut sb = String::with_capacity(160);
        if width > 0 {
            sb.push_str(&format!("{}-bit vectors ({width} B/op)", width * 8));
        } else {
            sb.push_str("NO hardware vectors - every vector path is running scalar");
        }
        sb.push_str(" [");
        let mut any = false;
        let mut add = |name: &str, supported: bool| {
            if !supported {
                return;
            }
            if any {
                sb.push(' ');
            }
            sb.push_str(name);
            any = true;
        };
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        {
            add("AVX512F", std::arch::is_x86_feature_detected!("avx512f"));
            add("AVX2", std::arch::is_x86_feature_detected!("avx2"));
            add("SSE4.2", std::arch::is_x86_feature_detected!("sse4.2"));
            add("BMI2", std::arch::is_x86_feature_detected!("bmi2"));
        }
        #[cfg(target_arch = "aarch64")]
        {
            add("NEON", std::arch::is_aarch64_feature_detected!("neon"));
            add("CRC32", std::arch::is_aarch64_feature_detected!("crc"));
        }
        if !any {
            sb.push_str("baseline only");
        }
        sb.push(']');
        sb.push_str(&format!(" dispatch={level:?}"));
        sb
    }
}
