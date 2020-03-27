extern crate encoding;
extern crate flate2;
extern crate nom;

use std::borrow::Cow;
use std::boxed::Box;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io;
use std::io::{Read, Seek, SeekFrom};

use encoding::label::encoding_from_whatwg_label;
use encoding::DecoderTrap;
use flate2::read::ZlibDecoder;
use nom::error::ErrorKind;
use nom::number::complete::{le_i16, le_i32, le_u32, le_u8};
use nom::IResult;
use nom::*;

const HEADER_MAGIC: &str = "ASSF (C) 2007 Aeomin DEV";

#[derive(Debug)]
pub struct ThorArchive<R: ?Sized> {
    obj: Box<R>,
    container: ThorContainer,
}

impl<R: Read + Seek> ThorArchive<R> {
    /// Create a new archive with the underlying object as the reader.
    pub fn new(mut obj: R) -> io::Result<ThorArchive<R>> {
        let mut buf = Vec::new();
        // TODO(LinkZ): Avoid using read_to_end, reading the whole file is unnecessary
        let _bytes_read = obj.read_to_end(&mut buf)?;
        let (_, thor_patch) = match parse_thor_patch(buf.as_slice()) {
            IResult::Ok(v) => v,
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Failed to parse archive.",
                ))
            }
        };
        Ok(ThorArchive {
            obj: Box::new(obj),
            container: thor_patch,
        })
    }

    pub fn file_count(&self) -> usize {
        self.container.header.file_count
    }

    pub fn target_grf_name(&self) -> String {
        self.container.header.target_grf_name.clone()
    }

    pub fn read_file_content<S: AsRef<str> + Hash>(&mut self, file_path: S) -> Option<Vec<u8>> {
        let file_entry = self.get_file_entry(file_path)?.clone();
        // Decompress the table with zlib
        match self.obj.seek(SeekFrom::Start(file_entry.offset)) {
            Ok(_) => (),
            Err(_) => return None,
        }
        let mut buf: Vec<u8> = Vec::with_capacity(file_entry.size_compressed);
        buf.resize(file_entry.size_compressed, 0);
        match self.obj.read_exact(buf.as_mut_slice()) {
            Ok(_) => (),
            Err(_) => return None,
        }
        let mut decoder = ZlibDecoder::new(&buf[..]);
        let mut decompressed_content = Vec::new();
        let _decompressed_size = match decoder.read_to_end(&mut decompressed_content) {
            Ok(v) => v,
            Err(_) => return None,
        };
        Some(decompressed_content)
    }

    pub fn get_file_entry<S: AsRef<str> + Hash>(&self, file_path: S) -> Option<&ThorEntry> {
        self.container.entries.get(file_path.as_ref())
    }

    pub fn get_entries(&self) -> impl Iterator<Item = &'_ ThorEntry> {
        self.container.entries.values()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct ThorContainer {
    pub header: ThorHeader,
    pub table: ThorTable,
    pub entries: HashMap<String, ThorEntry>,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ThorHeader {
    pub use_grf_merging: bool, // false -> client directory, true -> GRF
    pub file_count: usize,
    pub mode: ThorMode,
    pub target_grf_name: String, // If empty (size == 0) -> default GRF
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThorMode {
    SingleFile,
    MultipleFiles,
    Invalid,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ThorTable {
    SingleFile(SingleFileTableDesc),
    MultipleFiles(MultipleFilesTableDesc),
}

#[derive(Debug, PartialEq, Eq)]
pub struct SingleFileTableDesc {
    pub file_table_offset: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct MultipleFilesTableDesc {
    pub file_table_compressed_size: usize,
    pub file_table_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThorEntry {
    pub size_compressed: usize,
    pub size_decompressed: usize,
    pub relative_path: String,
    pub is_removed: bool,
    pub offset: u64,
}

impl Hash for ThorEntry {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.relative_path.hash(state);
    }
}

fn i16_to_mode(i: i16) -> ThorMode {
    match i {
        33 => ThorMode::SingleFile,
        48 => ThorMode::MultipleFiles,
        _ => ThorMode::Invalid,
    }
}

/// Checks entries' flags
/// If LSB is 1, the entry indicates a file deletion
fn is_file_removed(flags: u8) -> bool {
    (flags & 0b1) == 1
}

named!(parse_thor_header<&[u8], ThorHeader>,
    do_parse!(
        tag!(HEADER_MAGIC)
            >> use_grf_merging: le_u8
            >> file_count: le_u32
            >> mode: le_i16
            >> target_grf_name_size: le_u8
            >> target_grf_name: take_str!(target_grf_name_size)
            >> (ThorHeader {
                use_grf_merging: use_grf_merging == 1,
                file_count: (file_count - 1) as usize,
                mode: i16_to_mode(mode),
                target_grf_name: target_grf_name.to_string(),
            }
    )
));

named!(parse_single_file_table<&[u8], SingleFileTableDesc>,
    do_parse!(
        take!(1)
        >> (SingleFileTableDesc {
            file_table_offset: 0, // Offset in the 'data' field
        }
    )
));

named!(parse_multiple_files_table<&[u8], MultipleFilesTableDesc>,
    do_parse!(
        file_table_compressed_size: le_i32 
        >> file_table_offset: le_i32
        >> (MultipleFilesTableDesc {
            file_table_compressed_size: file_table_compressed_size as usize,
            file_table_offset: file_table_offset as u64, // Offset in the 'data' field
        }
    )
));

fn string_from_win_1252(v: &[u8]) -> Result<String, Cow<'static, str>> {
    let decoder = match encoding_from_whatwg_label("windows-1252") {
        Some(v) => v,
        None => return Err(Cow::Borrowed("Decoder unavailable")),
    };
    decoder.decode(v, DecoderTrap::Strict)
}

macro_rules! take_string_ansi (
    ( $i:expr, $size:expr ) => (
        {
            let input: &[u8] = $i;
            map_res!(input, take!($size), string_from_win_1252)
        }
     );
);

named!(parse_single_file_entry<&[u8], ThorEntry>,
    do_parse!(
        size_compressed: le_i32
        >> size_decompressed: le_i32
        >> relative_path_size: le_u8
        >> relative_path: take_string_ansi!(relative_path_size)
        >> (ThorEntry {
            size_compressed: size_compressed as usize,
            size_decompressed: size_decompressed as usize,
            relative_path: relative_path,
            is_removed: false,
            offset: 0,
        }
    )
));

/// Uses the given parser only if the flag is as expected
/// This is used to avoid parsing unexisting fields for files marked for deletion
macro_rules! take_if_not_removed (
    ( $i:expr, $parser:expr, $flags:expr ) => (
        {
            let input: &[u8] = $i;
            if is_file_removed($flags) {
                value!(input, 0)
            } else {
                $parser(input)
            }
        }
        );
);

named!(parse_multiple_files_entry<&[u8], ThorEntry>,
    do_parse!(
        relative_path_size: le_u8
        >> relative_path: take_string_ansi!(relative_path_size)
        >> flags: le_u8
        >> offset: take_if_not_removed!(le_u32, flags)
        >> size_compressed: take_if_not_removed!(le_i32, flags)
        >> size_decompressed: take_if_not_removed!(le_i32, flags)
        >> (ThorEntry {
            size_compressed: size_compressed as usize,
            size_decompressed: size_decompressed as usize,
            relative_path: relative_path,
            is_removed: is_file_removed(flags),
            offset: offset as u64,
        }
    )
));

named!(parse_multiple_files_entries<&[u8], HashMap<String, ThorEntry>>,
    fold_many1!(parse_multiple_files_entry, HashMap::new(), |mut acc: HashMap<_, _>, item| {
        acc.insert(item.relative_path.clone(), item);
        acc
    })
);

pub fn parse_thor_patch(input: &[u8]) -> IResult<&[u8], ThorContainer> {
    let (output, header) = parse_thor_header(input)?;
    match header.mode {
        ThorMode::Invalid => return Err(Err::Failure((input, ErrorKind::Switch))),
        ThorMode::SingleFile => {
            // Parse table
            let (output, table) = parse_single_file_table(output)?;
            // Parse the single entry
            let (output, entry) = parse_single_file_entry(output)?;
            return Ok((
                output,
                ThorContainer {
                    header: header,
                    table: ThorTable::SingleFile(table),
                    entries: [(entry.relative_path.clone(), entry)]
                        .iter()
                        .cloned()
                        .collect(),
                },
            ));
        }
        ThorMode::MultipleFiles => {
            let (output, mut table) = parse_multiple_files_table(output)?;
            let consumed_bytes = output.as_ptr() as u64 - input.as_ptr() as u64;
            if table.file_table_offset < consumed_bytes {
                return Err(Err::Failure((input, ErrorKind::Switch)));
            }
            // Compute actual table offset inside of 'output'
            table.file_table_offset -= consumed_bytes;
            // Decompress the table with zlib
            let mut decoder = ZlibDecoder::new(&output[table.file_table_offset as usize..]);
            let mut decompressed_table = Vec::new();
            let _decompressed_size = match decoder.read_to_end(&mut decompressed_table) {
                Ok(v) => v,
                Err(_) => return Err(Err::Failure((input, ErrorKind::Switch))),
            };
            // Parse multiple entries
            let (_output, entries) =
                match parse_multiple_files_entries(decompressed_table.as_slice()) {
                    Ok(v) => v,
                    Err(_) => return Err(Err::Failure((input, ErrorKind::Many1))),
                };
            return Ok((
                &[],
                ThorContainer {
                    header: header,
                    table: ThorTable::MultipleFiles(table),
                    entries: entries,
                },
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::path::PathBuf;

    #[test]
    fn test_open_thor_container() {
        let thor_dir_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/tests/thor");
        {
            let expected_content: HashMap<&str, usize> = [
                ("data.integrity", 63),
                (
                    "data\\texture\\\u{c0}\u{af}\u{c0}\u{fa}\u{c0}\u{ce}\u{c5}\u{cd}\u{c6}\u{e4}\u{c0}\u{cc}\u{bd}\u{ba}\\inventory\\icon_num.bmp",
                    560,
                ),
            ]
            .iter()
            .cloned()
            .collect();
            let check_tiny_thor_entries = |thor: &mut ThorArchive<File>| {
                let file_entries: Vec<ThorEntry> = thor.get_entries().map(|e| e.clone()).collect();
                for file_entry in file_entries {
                    let file_path: &str = &file_entry.relative_path[..];
                    assert!(expected_content.contains_key(file_path));
                    let expected_size = expected_content[file_path];
                    assert_eq!(file_entry.size_decompressed, expected_size);
                }
            };
            let thor_file_path = thor_dir_path.join("tiny.thor");
            let thor_file = File::open(thor_file_path).unwrap();
            let mut thor_archive = ThorArchive::new(thor_file).unwrap();
            assert_eq!(thor_archive.file_count(), expected_content.len());
            assert_eq!(thor_archive.target_grf_name(), "");
            check_tiny_thor_entries(&mut thor_archive);
        }

        {
            let thor_file_path = thor_dir_path.join("small.thor");
            let thor_file = File::open(thor_file_path).unwrap();
            let thor_archive = ThorArchive::new(thor_file).unwrap();
            assert_eq!(thor_archive.file_count(), 16);
            assert_eq!(thor_archive.target_grf_name(), "data.grf");
        }
    }
}
