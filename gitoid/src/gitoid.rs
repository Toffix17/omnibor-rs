//! A gitoid representing a single artifact.

use crate::Error;
use crate::HashAlgorithm;
use crate::HashRef;
use crate::ObjectType;
use crate::Result;
use core::fmt;
use core::fmt::Display;
use core::fmt::Formatter;
use core::hash::Hash;
use core::marker::PhantomData;
use core::ops::Not as _;
use digest::OutputSizeUser;
use generic_array::sequence::GenericSequence;
use generic_array::ArrayLength;
use generic_array::GenericArray;
use std::io::BufReader;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::str::Split;
use url::Url;

/// A struct that computes [gitoids][g] based on the selected algorithm
///
/// [g]: https://git-scm.com/book/en/v2/Git-Internals-Git-Objects
#[repr(C)]
#[derive(Clone, Copy, PartialOrd, Eq, Ord, Debug, Hash, PartialEq)]
pub struct GitOid<H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    #[doc(hidden)]
    _phantom: PhantomData<O>,

    #[doc(hidden)]
    value: GenericArray<u8, H::OutputSize>,
}

impl<H, O> Display for GitOid<H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", H::NAME, self.hash())
    }
}

impl<H, O> GitOid<H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    //===========================================================================================
    // Constructors
    //-------------------------------------------------------------------------------------------

    /// Helper constructor for building a [`GitOid`] from a parsed hash.
    fn new_from_hash(value: GenericArray<u8, H::OutputSize>) -> GitOid<H, O> {
        GitOid {
            _phantom: PhantomData,
            value,
        }
    }

    /// Create a new `GitOid` based on a slice of bytes.
    pub fn new_from_bytes(content: &[u8]) -> GitOid<H, O> {
        let digester = H::new();
        let reader = BufReader::new(content);
        let expected_length = content.len();

        // PANIC SAFETY: We're reading from an in-memory buffer, so no IO errors can arise.
        gitoid_from_buffer(digester, reader, expected_length).unwrap()
    }

    /// Create a `GitOid` from a UTF-8 string slice.
    pub fn new_from_str(s: &str) -> GitOid<H, O> {
        GitOid::new_from_bytes(s.as_bytes())
    }

    /// Create a `GitOid` from a reader.
    pub fn new_from_reader<R>(mut reader: R) -> Result<GitOid<H, O>>
    where
        R: Read + Seek,
    {
        let digester = H::new();
        let expected_length = stream_len(&mut reader)? as usize;
        gitoid_from_buffer(digester, reader, expected_length)
    }

    /// Construct a new `GitOid` from a `Url`.
    pub fn new_from_url(url: Url) -> Result<GitOid<H, O>> {
        url.try_into()
    }

    //===========================================================================================
    // Getters
    //-------------------------------------------------------------------------------------------

    /// Get a URL for the current `GitOid`.
    pub fn url(&self) -> Url {
        let s = format!("gitoid:{}:{}:{}", O::NAME, H::NAME, self.hash());
        // PANIC SAFETY: We know that this is a valid URL.
        Url::parse(&s).unwrap()
    }

    /// Get the hash data as a slice of bytes.
    pub fn hash(&self) -> HashRef<'_> {
        HashRef::new(&self.value[..])
    }

    /// Get the hash algorithm used for the `GitOid`.
    pub fn hash_algorithm(&self) -> &'static str {
        H::NAME
    }

    /// Get the object type of the `GitOid`.
    pub fn object_type(&self) -> &'static str {
        O::NAME
    }

    /// Get the length of the hash in bytes.
    pub fn hash_len(&self) -> usize {
        <H as OutputSizeUser>::output_size()
    }
}

struct GitOidUrlParser<'u, H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    url: &'u Url,

    segments: Split<'u, char>,

    #[doc(hidden)]
    _hash_algorithm: PhantomData<H>,

    #[doc(hidden)]
    _object_type: PhantomData<O>,
}

fn some_if_not_empty(s: &str) -> Option<&str> {
    s.is_empty().not().then_some(s)
}

impl<'u, H, O> GitOidUrlParser<'u, H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    fn new(url: &'u Url) -> GitOidUrlParser<'u, H, O> {
        GitOidUrlParser {
            url,
            segments: url.path().split(':'),
            _hash_algorithm: PhantomData,
            _object_type: PhantomData,
        }
    }

    fn parse(&mut self) -> Result<GitOid<H, O>> {
        self.validate_url_scheme()
            .and_then(|_| self.validate_object_type())
            .and_then(|_| self.validate_hash_algorithm())
            .and_then(|_| self.parse_hash())
            .map(GitOid::new_from_hash)
    }

    fn validate_url_scheme(&self) -> Result<()> {
        if self.url.scheme() != "gitoid" {
            return Err(Error::InvalidScheme(self.url.clone()));
        }

        Ok(())
    }

    fn validate_object_type(&mut self) -> Result<()> {
        let object_type = self
            .segments
            .next()
            .and_then(some_if_not_empty)
            .ok_or_else(|| Error::MissingObjectType(self.url.clone()))?;

        if object_type != O::NAME {
            return Err(Error::MismatchedObjectType {
                expected: O::NAME.to_string(),
                observed: object_type.to_string(),
            });
        }

        Ok(())
    }

    fn validate_hash_algorithm(&mut self) -> Result<()> {
        let hash_algorithm = self
            .segments
            .next()
            .and_then(some_if_not_empty)
            .ok_or_else(|| Error::MissingHashAlgorithm(self.url.clone()))?;

        if hash_algorithm != H::NAME {
            return Err(Error::MismatchedHashAlgorithm {
                expected: H::NAME.to_string(),
                observed: hash_algorithm.to_string(),
            });
        }

        Ok(())
    }

    fn parse_hash(&mut self) -> Result<GenericArray<u8, H::OutputSize>> {
        let hex_str = self
            .segments
            .next()
            .and_then(some_if_not_empty)
            .ok_or_else(|| Error::MissingHash(self.url.clone()))?;

        // TODO(abrinker): When `sha1` et al. move to generic-array 1.0, update this to use the `arr!` macro.
        let mut value = GenericArray::generate(|_| 0);
        hex::decode_to_slice(hex_str, &mut value)?;

        let expected_size = <H as OutputSizeUser>::output_size();
        if value.len() != expected_size {
            return Err(Error::UnexpectedHashLength {
                expected: expected_size,
                observed: value.len(),
            });
        }

        Ok(value)
    }
}

impl<H, O> TryFrom<Url> for GitOid<H, O>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
{
    type Error = Error;

    fn try_from(url: Url) -> Result<GitOid<H, O>> {
        GitOidUrlParser::new(&url).parse()
    }
}

/// Take a `BufReader` and generate a hash based on the `GitOid`'s hashing algorithm.
///
/// Will return an `Err` if the `BufReader` generates an `Err` or if the
/// `expected_length` is different from the actual length.
///
/// Why the latter `Err`?
///
/// The prefix string includes the number of bytes being hashed and that's the
/// `expected_length`. If the actual bytes hashed differs, then something went
/// wrong and the hash is not valid.
fn gitoid_from_buffer<H, O, R>(
    mut digester: H,
    mut reader: R,
    expected_length: usize,
) -> Result<GitOid<H, O>>
where
    H: HashAlgorithm,
    O: ObjectType,
    <H as OutputSizeUser>::OutputSize: ArrayLength<u8>,
    GenericArray<u8, H::OutputSize>: Copy,
    R: Read,
{
    let prefix = format!("{} {}\0", O::NAME, expected_length);

    // Linux default page size is 4096, so use that.
    let mut buf = [0; 4096];
    let mut amount_read: usize = 0;

    digester.update(prefix.as_bytes());

    loop {
        match reader.read(&mut buf)? {
            0 => break,

            size => {
                digester.update(&buf[..size]);
                amount_read += size;
            }
        }
    }

    if amount_read != expected_length {
        return Err(Error::BadLength {
            expected: expected_length,
            actual: amount_read,
        });
    }

    let hash = digester.finalize();
    let expected_size = <H as OutputSizeUser>::output_size();

    if hash.len() != expected_size {
        return Err(Error::UnexpectedHashLength {
            expected: expected_size,
            observed: hash.len(),
        });
    }

    Ok(GitOid::new_from_hash(hash))
}

// Adapted from the Rust standard library's unstable implementation
// of `Seek::stream_len`.
//
// TODO(abrinker): Remove this when `Seek::stream_len` is stabilized.
//
// License reproduction:
//
// Permission is hereby granted, free of charge, to any
// person obtaining a copy of this software and associated
// documentation files (the "Software"), to deal in the
// Software without restriction, including without
// limitation the rights to use, copy, modify, merge,
// publish, distribute, sublicense, and/or sell copies of
// the Software, and to permit persons to whom the Software
// is furnished to do so, subject to the following
// conditions:
//
// The above copyright notice and this permission notice
// shall be included in all copies or substantial portions
// of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
// ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
// TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
// PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
// SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
// CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
// OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
// IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
// DEALINGS IN THE SOFTWARE.
fn stream_len<R>(mut stream: R) -> Result<u64>
where
    R: Seek,
{
    let old_pos = stream.stream_position()?;
    let len = stream.seek(SeekFrom::End(0))?;

    // Avoid seeking a third time when we were already at the end of the
    // stream. The branch is usually way cheaper than a seek operation.
    if old_pos != len {
        stream.seek(SeekFrom::Start(old_pos))?;
    }

    Ok(len)
}
