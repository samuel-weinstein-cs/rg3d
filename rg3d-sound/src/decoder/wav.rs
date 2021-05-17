use crate::{buffer::DataSource, error::SoundError};
use hound::{WavReader, WavSamples};

pub(in crate) struct WavDecoder {
    reader: WavReader<DataSource>,
    // samples: WavSamples<f32>,
}

impl Iterator for WavDecoder {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        todo!()
    }
}

impl<'a> WavDecoder {
    pub fn new(source: DataSource) -> Result<Self, DataSource> {
        let mut reader = WavReader::new(source);
        match reader {
            Ok(reader) => {
                // let samples = reader.samples::<'a, f32>();
                // let () = samples;
                Ok(Self { reader })
            }
            Err(_) => Err(source),
        }
    }
}
