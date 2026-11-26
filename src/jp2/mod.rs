use std::vec::Vec;

use crate::cancel::Cancel;
use crate::error::{FormatError, Result, bail};
use crate::j2c::ComponentData;
use crate::jp2::r#box::{FILE_TYPE, JP2_SIGNATURE};
use crate::reader::BitReader;
use crate::{DecodeSettings, Image};

pub(crate) mod r#box;

/// A decoded JPEG2000 image.
pub struct DecodedImage<'a> {
    pub(crate) decoded_components: &'a mut Vec<ComponentData>,
    pub(crate) cancel: Cancel<'a>,
}

pub(crate) fn parse<'a>(data: &'a [u8], settings: DecodeSettings) -> Result<Image<'a>> {
    let mut reader = BitReader::new(data);
    let signature_box = r#box::read(&mut reader, settings.strict).ok_or(FormatError::InvalidBox)?;

    if signature_box.box_type != JP2_SIGNATURE {
        bail!(FormatError::InvalidSignature);
    }

    let file_type_box = r#box::read(&mut reader, settings.strict).ok_or(FormatError::InvalidBox)?;

    if file_type_box.box_type != FILE_TYPE {
        bail!(FormatError::InvalidFileType);
    }

    let mut has_header = false;
    let mut parsed_codestream = None;

    while !reader.at_end() {
        let Some(current_box) = r#box::read(&mut reader, settings.strict) else {
            if settings.strict {
                bail!(FormatError::InvalidBox);
            }

            break;
        };

        match current_box.box_type {
            r#box::JP2_HEADER => {
                has_header = true;
            }
            r#box::CONTIGUOUS_CODESTREAM => {
                parsed_codestream = Some(crate::j2c::parse_raw(current_box.data, &settings)?);
            }
            _ => {}
        }
    }

    if !has_header {
        bail!(FormatError::InvalidBox);
    }

    let parsed_codestream = parsed_codestream.ok_or(FormatError::MissingCodestream)?;

    Ok(Image {
        codestream: parsed_codestream.data,
        header: parsed_codestream.header,
        settings,
    })
}
