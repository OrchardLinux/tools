//! AOP firmware containers: a binary serialized dictionary whose `firmware`
//! array holds {header: {coprocessor: ROLE, …}, data: IMAGE} entries.
use crate::osserialize::{self, Value};
use crate::{Error, Result};

/// The image for one coprocessor role, such as `AOP` or `AOP_ext`.
pub fn image(container: &[u8], coprocessor: &str) -> Result<Vec<u8>> {
    let doc = osserialize::binary(container)?;
    let entries = doc
        .get("firmware")
        .and_then(Value::as_array)
        .ok_or(Error("AOP container has no firmware list"))?;
    let mut images = entries.iter().filter_map(|e| {
        let role = e.get("header")?.get("coprocessor")?.as_str()?;
        (role == coprocessor).then(|| e.get("data").and_then(Value::as_data))
    });
    match (images.next(), images.next()) {
        (Some(Some(data)), None) => Ok(data.to_vec()),
        _ => Err(Error(
            "AOP container does not hold exactly one image for this role",
        )),
    }
}
