use crate::reader::BitReader;

pub(crate) const JP2_SIGNATURE: u32 = 0x6A502020;
pub(crate) const FILE_TYPE: u32 = 0x66747970;
pub(crate) const JP2_HEADER: u32 = 0x6A703268;
pub(crate) const CONTIGUOUS_CODESTREAM: u32 = 0x6A703263;

pub(crate) struct Jp2Box<'a> {
    pub(crate) data: &'a [u8],
    pub(crate) box_type: u32,
}

pub(crate) fn read<'a>(reader: &mut BitReader<'a>, strict: bool) -> Option<Jp2Box<'a>> {
    let l_box = reader.read_u32()?;
    let t_box = reader.read_u32()?;

    let data = match l_box {
        0 => {
            let data = reader.tail()?;
            reader.jump_to_end();
            data
        }
        1 => {
            let xl_box = reader.read_u64()?.checked_sub(16)?;
            read_data(reader, xl_box as usize, strict)?
        }
        _ => {
            let length = l_box.checked_sub(8)?;
            read_data(reader, length as usize, strict)?
        }
    };

    Some(Jp2Box {
        data,
        box_type: t_box,
    })
}

fn read_data<'a>(reader: &mut BitReader<'a>, length: usize, strict: bool) -> Option<&'a [u8]> {
    if let Some(data) = reader.read_bytes(length) {
        return Some(data);
    }

    // Some files declare a box length that exceeds the file, in which case
    // we use the remaining data instead.
    if strict {
        return None;
    }

    let data = reader.tail()?;
    reader.jump_to_end();

    Some(data)
}
