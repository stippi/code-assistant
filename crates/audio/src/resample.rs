//! Linear resampling between device rates and 24 kHz. Plenty for speech.

/// Streaming linear resampler; keeps its phase across calls.
pub struct Resampler {
    /// Input samples per output sample.
    step: f64,
    /// Position of the next output sample, relative to `last`.
    position: f64,
    last: f32,
}

impl Resampler {
    pub fn new(from_rate: u32, to_rate: u32) -> Self {
        Self {
            step: from_rate as f64 / to_rate as f64,
            position: 0.0,
            last: 0.0,
        }
    }

    /// Resample a chunk of mono input, appending to `out`.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        for &sample in input {
            // Emit every output position between `last` (0.0) and `sample`
            // (1.0).
            while self.position <= 1.0 {
                let t = self.position as f32;
                out.push(self.last + (sample - self.last) * t);
                self.position += self.step;
            }
            self.position -= 1.0;
            self.last = sample;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_ratio_sets_the_output_length() {
        let input = vec![0.5; 4800];
        let mut out = Vec::new();
        Resampler::new(48_000, 24_000).process(&input, &mut out);
        assert!((out.len() as i64 - 2400).abs() <= 2, "{}", out.len());

        let mut up = Vec::new();
        Resampler::new(24_000, 48_000).process(&input, &mut up);
        assert!((up.len() as i64 - 9600).abs() <= 2, "{}", up.len());
    }

    #[test]
    fn phase_carries_across_chunks() {
        let input: Vec<f32> = (0..441).map(|i| i as f32).collect();
        let mut whole = Vec::new();
        Resampler::new(44_100, 24_000).process(&input, &mut whole);
        let mut chunked = Vec::new();
        let mut resampler = Resampler::new(44_100, 24_000);
        for chunk in input.chunks(37) {
            resampler.process(chunk, &mut chunked);
        }
        assert_eq!(whole, chunked);
    }
}
