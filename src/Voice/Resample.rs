pub struct Resampler {
    Channels: usize,
    Step: f64,
    Buffer: Vec<f32>,
    Position: f64,
}

impl Resampler {
    pub fn New(FromRate: u32, ToRate: u32, Channels: usize) -> Self {
        let Channels = Channels.max(1);
        Resampler { Channels, Step: FromRate as f64 / ToRate as f64, Buffer: vec![0.0; Channels], Position: 1.0 }
    }

    pub fn IsPassthrough(&self) -> bool {
        (self.Step - 1.0).abs() < 1e-9
    }

    pub fn Process(&mut self, Input: &[f32], Out: &mut Vec<f32>) {
        if self.IsPassthrough() {
            Out.extend_from_slice(Input);
            return;
        }
        let Channels = self.Channels;
        self.Buffer.extend_from_slice(Input);
        let Frames = self.Buffer.len() / Channels;
        while self.Position + 2.0 < Frames as f64 {
            let Index = self.Position as usize;
            let Fraction = (self.Position - Index as f64) as f32;
            for Channel in 0..Channels {
                let Sample = |Offset: usize| self.Buffer[Offset * Channels + Channel];
                let (P0, P1, P2, P3) = (Sample(Index - 1), Sample(Index), Sample(Index + 1), Sample(Index + 2));
                Out.push(
                    P1 + 0.5
                        * Fraction
                        * (P2 - P0
                            + Fraction
                                * (2.0 * P0 - 5.0 * P1 + 4.0 * P2 - P3 + Fraction * (3.0 * (P1 - P2) + P3 - P0))),
                );
            }
            self.Position += self.Step;
        }
        let Consumed = (self.Position as usize).saturating_sub(1).min(Frames);
        self.Buffer.drain(..Consumed * Channels);
        self.Position -= Consumed as f64;
    }
}

#[cfg(test)]
mod Tests {
    use super::*;

    #[test]
    fn LengthTracksRatio() {
        let mut Converter = Resampler::New(44_100, 48_000, 2);
        let mut Out = Vec::new();
        for _ in 0..100 {
            Converter.Process(&vec![0.25; 441 * 2], &mut Out);
        }
        let Frames = Out.len() / 2;
        assert!((Frames as i64 - 48_000).abs() < 8, "{Frames}");
        assert!(Out[1000..].iter().all(|Value| (Value - 0.25).abs() < 1e-4));
    }

    #[test]
    fn SineSurvives() {
        let mut Converter = Resampler::New(48_000, 44_100, 1);
        let Input: Vec<f32> =
            (0..48_000).map(|Index| (Index as f32 * 440.0 * std::f32::consts::TAU / 48_000.0).sin()).collect();
        let mut Out = Vec::new();
        for Chunk in Input.chunks(480) {
            Converter.Process(Chunk, &mut Out);
        }
        for (Index, Value) in Out.iter().enumerate().skip(100).take(40_000) {
            let Want = (Index as f64 * 440.0 * std::f64::consts::TAU / 44_100.0).sin() as f32;
            assert!((Value - Want).abs() < 0.02, "k={Index} got {Value} want {Want}");
        }
    }
}
