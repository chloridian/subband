use crate::error::{FormatError, Result};
use crate::jp2::ImageBoxes;
use crate::reader::BitReader;

pub(crate) fn parse(boxes: &mut ImageBoxes, data: &[u8]) -> Result<()> {
    if boxes.color_specification.is_some() {
        return Ok(());
    }

    let mut reader = BitReader::new(data);

    let meth = reader.read_byte().ok_or(FormatError::InvalidBox)?;
    let _prec = reader.read_byte().ok_or(FormatError::InvalidBox)?;
    let _approx = reader.read_byte().ok_or(FormatError::InvalidBox)?;

    let method = match meth {
        1 => {
            let enumerated = reader.read_u32().ok_or(FormatError::InvalidBox)?;
            ColorSpace::Enumerated(EnumeratedColorspace::from_raw(enumerated))
        }
        2 => ColorSpace::Icc,
        _ => ColorSpace::Unknown,
    };

    boxes.color_specification = Some(ColorSpecificationBox {
        color_space: method,
    });

    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct ColorSpecificationBox {
    pub(crate) color_space: ColorSpace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ColorSpace {
    Enumerated(EnumeratedColorspace),
    Icc,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnumeratedColorspace {
    Srgb,
    Greyscale,
    EsRgb,
    Other,
}

impl EnumeratedColorspace {
    fn from_raw(value: u32) -> Self {
        match value {
            16 => Self::Srgb,
            17 => Self::Greyscale,
            20 => Self::EsRgb,
            _ => Self::Other,
        }
    }
}
