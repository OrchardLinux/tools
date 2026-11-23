//! Random access to bytes, wherever they live.
use crate::{Code, Context, Error, Result};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

/// Bytes of a known size that can be read at any offset.
pub trait Source {
    fn size(&self) -> u64;

    /// Reads exactly `len` bytes at `offset`. Callers use [`Source::read_at`],
    /// which checks the range first.
    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>>;

    fn read_at(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        if offset
            .checked_add(len as u64)
            .is_none_or(|end| end > self.size())
        {
            return Err(Error::new(Code::Format, "read past the end of the source"));
        }
        let data = self.fetch(offset, len)?;
        if data.len() != len {
            return Err(Error::new(
                Code::Io,
                "source returned the wrong number of bytes",
            ));
        }
        Ok(data)
    }
}

impl<S: Source + ?Sized> Source for &mut S {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        (**self).fetch(offset, len)
    }
}

impl<S: Source + ?Sized> Source for Box<S> {
    fn size(&self) -> u64 {
        (**self).size()
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        (**self).fetch(offset, len)
    }
}

impl Source for Vec<u8> {
    fn size(&self) -> u64 {
        self.len() as u64
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        Ok(self[offset as usize..offset as usize + len].to_vec())
    }
}

/// A local file.
pub struct FileSource {
    file: File,
    size: u64,
}

impl FileSource {
    pub fn open(path: &Path) -> Result<FileSource> {
        let file = File::open(path).context(path.display())?;
        let size = file.metadata().context(path.display())?.len();
        Ok(FileSource { file, size })
    }
}

impl Source for FileSource {
    fn size(&self) -> u64 {
        self.size
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.file.seek(SeekFrom::Start(offset))?;
        let mut data = vec![0; len];
        self.file.read_exact(&mut data)?;
        Ok(data)
    }
}

/// A window into another source, such as one member of an archive.
pub struct Slice<S> {
    source: S,
    base: u64,
    size: u64,
}

impl<S: Source> Slice<S> {
    pub fn new(source: S, base: u64, size: u64) -> Result<Slice<S>> {
        if base.checked_add(size).is_none_or(|end| end > source.size()) {
            return Err(Error::new(
                Code::Format,
                "slice extends past the end of the source",
            ));
        }
        Ok(Slice { source, base, size })
    }
}

impl<S: Source> Source for Slice<S> {
    fn size(&self) -> u64 {
        self.size
    }

    fn fetch(&mut self, offset: u64, len: usize) -> Result<Vec<u8>> {
        self.source.read_at(self.base + offset, len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slice_stays_inside_its_window() {
        let data: Vec<u8> = (0..100).collect();
        let mut slice = Slice::new(data, 10, 20).unwrap();
        assert_eq!(slice.read_at(5, 3).unwrap(), [15, 16, 17]);
        assert!(slice.read_at(18, 3).is_err());
        assert!(Slice::new(vec![0; 10], 5, 6).is_err());
    }
}
