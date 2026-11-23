//! The payload of an IM4P container, which Apple uses for firmware files.
use crate::der::{self, IA5_STRING, OCTET_STRING, SEQUENCE};
use crate::{Error, Result};

/// The payload bytes as stored in the container.
pub fn payload(bytes: &[u8]) -> Result<&[u8]> {
    let outer = der::read(bytes)?;
    if outer.tag != SEQUENCE {
        return Err(Error("not an IM4P container"));
    }
    let fields = der::items(outer.body)?;
    match fields.as_slice() {
        [magic, _, _, data, ..]
            if magic.tag == IA5_STRING && magic.body == b"IM4P" && data.tag == OCTET_STRING =>
        {
            Ok(data.body)
        }
        _ => Err(Error("not an IM4P container")),
    }
}

/// The unique production AES-256 keybag of a complete, typed IM4P.
/// The caller authenticates the container before using this request on USB.
pub fn keybag(bytes: &[u8], kind: &[u8; 4]) -> Result<[u8; 48]> {
    let outer = der::read(bytes)?;
    if outer.tag != SEQUENCE || !outer.rest.is_empty() {
        return Err(Error("invalid IM4P container"));
    }
    let fields = der::items(outer.body)?;
    if !(5..=6).contains(&fields.len())
        || fields[0].ia5()? != "IM4P"
        || fields[1].tag != IA5_STRING
        || fields[1].body != kind
        || fields[2].tag != IA5_STRING
        || fields[3].tag != OCTET_STRING
        || fields[3].body.is_empty()
        || !fields[3].body.len().is_multiple_of(16)
        || fields[4].tag != OCTET_STRING
        || (fields.len() == 6 && fields[5].tag != SEQUENCE)
    {
        return Err(Error("invalid encrypted IM4P fields"));
    }
    let bags = fields[4].only_child()?;
    if bags.tag != SEQUENCE {
        return Err(Error("invalid IM4P keybags"));
    }
    let mut found = None;
    let mut seen = [false; 2];
    for bag in der::items(bags.body)? {
        if bag.tag != SEQUENCE {
            return Err(Error("invalid IM4P keybag"));
        }
        let values = der::items(bag.body)?;
        if values.len() != 3 {
            return Err(Error("invalid IM4P keybag fields"));
        }
        let index = match (values[0].tag, values[0].body) {
            (der::INTEGER, [1]) => 0,
            (der::INTEGER, [2]) => 1,
            _ => return Err(Error("unsupported IM4P keybag type")),
        };
        if seen[index]
            || values[1].tag != OCTET_STRING
            || values[1].body.len() != 16
            || values[2].tag != OCTET_STRING
            || values[2].body.len() != 32
        {
            return Err(Error("invalid or ambiguous IM4P keybag"));
        }
        seen[index] = true;
        if index == 0 {
            let mut bag = [0; 48];
            bag[..16].copy_from_slice(values[1].body);
            bag[16..].copy_from_slice(values[2].body);
            found = Some(bag);
        }
    }
    found.ok_or(Error("production IM4P keybag missing"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::write;

    fn image(bags: &[u8]) -> Vec<u8> {
        let body = [
            write(IA5_STRING, b"IM4P"),
            write(IA5_STRING, b"ibss"),
            write(IA5_STRING, b""),
            write(OCTET_STRING, &[0; 16]),
            write(OCTET_STRING, &write(SEQUENCE, bags)),
        ]
        .concat();
        write(SEQUENCE, &body)
    }

    #[test]
    fn production_bag_is_typed_bounded_and_unambiguous() {
        let bag = |n| {
            write(
                SEQUENCE,
                &[
                    write(der::INTEGER, &[n]),
                    write(OCTET_STRING, &[3; 16]),
                    write(OCTET_STRING, &[4; 32]),
                ]
                .concat(),
            )
        };
        let bytes = image(&[bag(2), bag(1)].concat());
        assert_eq!(&keybag(&bytes, b"ibss").unwrap()[..16], &[3; 16]);
        assert!(keybag(&bytes, b"ibec").is_err());
        for bags in [vec![], bag(2), bag(3), [bag(1), bag(1)].concat()] {
            assert!(keybag(&image(&bags), b"ibss").is_err());
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(keybag(&trailing, b"ibss").is_err());
        for end in 0..bytes.len() {
            assert!(keybag(&bytes[..end], b"ibss").is_err());
        }
    }

    #[test]
    fn reads_the_payload() {
        let mut body = write(IA5_STRING, b"IM4P");
        body.extend(write(IA5_STRING, b"mtfw"));
        body.extend(write(IA5_STRING, b"desc"));
        body.extend(write(OCTET_STRING, b"payload"));
        assert_eq!(payload(&write(SEQUENCE, &body)).unwrap(), b"payload");
        assert!(payload(&write(SEQUENCE, &write(IA5_STRING, b"IMG4"))).is_err());
    }
}
