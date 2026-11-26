use std::vec::Vec;

use crate::cancel::Cancel;
use crate::error::{DecodeError, Result, bail};
use crate::j2c::ComponentData;
use crate::jp2::r#box::{FILE_TYPE, JP2_SIGNATURE};
use crate::reader::BitReader;
use crate::{DecodeSettings, Image};

pub(crate) mod r#box;

const MAX_BOXES_PER_LEVEL: usize = 64;

pub struct DecodedImage<'a> {
    pub(crate) decoded_components: &'a mut Vec<ComponentData>,
    pub(crate) cancel: Cancel<'a>,
}

pub(crate) fn parse<'a>(data: &'a [u8], settings: DecodeSettings) -> Result<Image<'a>> {
    let mut reader = BitReader::new(data);
    let signature_box = r#box::read(&mut reader, settings.strict).ok_or(DecodeError::Malformed)?;

    if signature_box.box_type != JP2_SIGNATURE {
        bail!(DecodeError::Malformed);
    }

    let file_type_box = r#box::read(&mut reader, settings.strict).ok_or(DecodeError::Malformed)?;

    if file_type_box.box_type != FILE_TYPE {
        bail!(DecodeError::Malformed);
    }

    let mut box_count = 2_usize;
    let mut has_header = false;
    let mut parsed_codestream = None;

    while !reader.at_end() {
        box_count += 1;
        if box_count > MAX_BOXES_PER_LEVEL {
            bail!(DecodeError::TooLarge);
        }

        let Some(current_box) = r#box::read(&mut reader, settings.strict) else {
            if settings.strict {
                bail!(DecodeError::Malformed);
            }

            break;
        };

        match current_box.box_type {
            r#box::JP2_HEADER => {
                has_header = true;
                validate_header_boxes(current_box.data, settings.strict)?;
            }
            r#box::PALETTE | r#box::COMPONENT_MAPPING => {
                bail!(DecodeError::Unsupported);
            }
            r#box::CONTIGUOUS_CODESTREAM => {
                parsed_codestream = Some(crate::j2c::parse_raw(current_box.data, &settings)?);
            }
            _ => {}
        }
    }

    if !has_header {
        bail!(DecodeError::Malformed);
    }

    let parsed_codestream = parsed_codestream.ok_or(DecodeError::Malformed)?;

    Ok(Image {
        codestream: parsed_codestream.data,
        header: parsed_codestream.header,
        settings,
    })
}

fn validate_header_boxes(data: &[u8], strict: bool) -> Result<()> {
    let mut reader = BitReader::new(data);
    let mut count = 0_usize;

    while !reader.at_end() {
        count += 1;
        if count > MAX_BOXES_PER_LEVEL {
            bail!(DecodeError::TooLarge);
        }

        let Some(current_box) = r#box::read(&mut reader, strict) else {
            if strict {
                bail!(DecodeError::Malformed);
            }

            break;
        };

        if matches!(
            current_box.box_type,
            r#box::PALETTE | r#box::COMPONENT_MAPPING
        ) {
            bail!(DecodeError::Unsupported);
        }
    }

    Ok(())
}
