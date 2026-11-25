//! Item ids: `128` (brief), `c12` (chore), `b41` (backlog item).

use std::fmt;
use std::str::FromStr;

/// The two list files of a project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ListKind {
    Chores,
    Backlog,
}

impl ListKind {
    pub const ALL: [ListKind; 2] = [ListKind::Chores, ListKind::Backlog];

    /// The letter in front of every id of this list.
    pub const fn prefix(self) -> char {
        match self {
            ListKind::Chores => 'c',
            ListKind::Backlog => 'b',
        }
    }

    /// The directory of a project its items' files are in.
    pub const fn dir_name(self) -> &'static str {
        match self {
            ListKind::Chores => "chores",
            ListKind::Backlog => "backlog",
        }
    }

    /// The single file the kind's items were in before they were files of
    /// their own (2026-11-25): its history still holds ids ever used, and
    /// `migrate` converts one still there.
    pub const fn file_name(self) -> &'static str {
        match self {
            ListKind::Chores => "chores.md",
            ListKind::Backlog => "backlog.md",
        }
    }

    /// The one definition of a list id: the prefix, then `[1-9][0-9]*`, at
    /// most `u32::MAX`.
    ///
    /// Leading zeros and overflow are not ids, so every number has one
    /// spelling and an id is found by searching for it. A file name that
    /// fails here (`chores/c012.md`, `chores/b41.md`) is no item's.
    pub fn parse_id(self, word: &[u8]) -> Option<u32> {
        match word {
            [prefix, digits @ ..] if *prefix == self.prefix() as u8 => match digits {
                [b'1'..=b'9', ..] => parse_digits(digits),
                _ => None,
            },
            _ => None,
        }
    }
}

/// Non-empty ASCII digits with a value up to `u32::MAX`. Not `str::parse`,
/// which also takes a sign.
fn parse_digits(digits: &[u8]) -> Option<u32> {
    if digits.is_empty() {
        return None;
    }
    digits.iter().try_fold(0u32, |n, b| match b {
        b'0'..=b'9' => n.checked_mul(10)?.checked_add(u32::from(b - b'0')),
        _ => None,
    })
}

/// A brief's number, per project.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BriefId(u32);

impl BriefId {
    pub const fn new(number: u32) -> BriefId {
        BriefId(number)
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    /// The one definition of a brief number, for an argument and for the
    /// leading digits of a file name alike.
    ///
    /// Leading zeros count (`007` is 7), unlike in a list id: the number is
    /// also read off file names the tool did not write (an archive copied
    /// into `briefs/done/`), and two spellings of one number must come out as
    /// the same brief for lint to report them.
    pub fn parse(digits: &[u8]) -> Option<BriefId> {
        parse_digits(digits).map(BriefId)
    }
}

impl fmt::Display for BriefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for BriefId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<BriefId, InvalidId> {
        BriefId::parse(s.as_bytes()).ok_or_else(|| InvalidId::new(s, Wanted::Brief))
    }
}

/// `ChoreId` and `BacklogId` differ in their list only. Two types, so that
/// `done b41` fails where the argument is parsed; inside the crate a list id
/// is a `(ListKind, u32)`.
macro_rules! list_id {
    ($(#[$doc:meta])* $name:ident, $kind:expr, $wanted:expr) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u32);

        impl $name {
            pub const KIND: ListKind = $kind;

            /// `None` for 0, which the id grammar cannot spell.
            pub const fn new(number: u32) -> Option<$name> {
                if number == 0 { None } else { Some($name(number)) }
            }

            pub const fn get(self) -> u32 {
                self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}{}", Self::KIND.prefix(), self.0)
            }
        }

        impl FromStr for $name {
            type Err = InvalidId;

            fn from_str(s: &str) -> Result<$name, InvalidId> {
                Self::KIND
                    .parse_id(s.as_bytes())
                    .map($name)
                    .ok_or_else(|| InvalidId::new(s, $wanted))
            }
        }
    };
}

list_id!(
    /// A chore's id (`c12`).
    ChoreId,
    ListKind::Chores,
    Wanted::Chore
);
list_id!(
    /// A backlog item's id (`b41`).
    BacklogId,
    ListKind::Backlog,
    Wanted::Backlog
);

/// Any item's id, where a command takes all three kinds (`show`, `--from`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ItemId {
    Brief(BriefId),
    Chore(ChoreId),
    Backlog(BacklogId),
}

impl fmt::Display for ItemId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ItemId::Brief(id) => id.fmt(f),
            ItemId::Chore(id) => id.fmt(f),
            ItemId::Backlog(id) => id.fmt(f),
        }
    }
}

impl FromStr for ItemId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<ItemId, InvalidId> {
        // The three grammars differ in their first byte, so the order of the
        // attempts decides nothing.
        if let Ok(id) = s.parse() {
            Ok(ItemId::Brief(id))
        } else if let Ok(id) = s.parse() {
            Ok(ItemId::Chore(id))
        } else if let Ok(id) = s.parse() {
            Ok(ItemId::Backlog(id))
        } else {
            Err(InvalidId::new(s, Wanted::Item))
        }
    }
}

/// A chore's or a backlog item's id, where a command takes either but no
/// brief (`move`: briefs have no order of their own, the board ranks them by
/// status).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ListId {
    Chore(ChoreId),
    Backlog(BacklogId),
}

impl ListId {
    /// The id of item `number` of a kind; `None` for 0.
    pub fn of(kind: ListKind, number: u32) -> Option<ListId> {
        match kind {
            ListKind::Chores => ChoreId::new(number).map(ListId::Chore),
            ListKind::Backlog => BacklogId::new(number).map(ListId::Backlog),
        }
    }

    pub const fn kind(self) -> ListKind {
        match self {
            ListId::Chore(_) => ListKind::Chores,
            ListId::Backlog(_) => ListKind::Backlog,
        }
    }

    pub const fn get(self) -> u32 {
        match self {
            ListId::Chore(id) => id.get(),
            ListId::Backlog(id) => id.get(),
        }
    }
}

impl From<ListId> for ItemId {
    fn from(id: ListId) -> ItemId {
        match id {
            ListId::Chore(id) => ItemId::Chore(id),
            ListId::Backlog(id) => ItemId::Backlog(id),
        }
    }
}

impl fmt::Display for ListId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ListId::Chore(id) => id.fmt(f),
            ListId::Backlog(id) => id.fmt(f),
        }
    }
}

impl FromStr for ListId {
    type Err = InvalidId;

    fn from_str(s: &str) -> Result<ListId, InvalidId> {
        if let Ok(id) = s.parse() {
            Ok(ListId::Chore(id))
        } else if let Ok(id) = s.parse() {
            Ok(ListId::Backlog(id))
        } else {
            Err(InvalidId::new(s, Wanted::ListItem))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wanted {
    Brief,
    Chore,
    Backlog,
    ListItem,
    Item,
}

/// A word that is not the id a command asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidId {
    word: String,
    wanted: Wanted,
}

impl InvalidId {
    fn new(word: &str, wanted: Wanted) -> InvalidId {
        InvalidId {
            word: word.to_owned(),
            wanted,
        }
    }
}

impl fmt::Display for InvalidId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let wanted = match self.wanted {
            Wanted::Brief => "a brief number like 128",
            Wanted::Chore => "a chore id like c12",
            Wanted::Backlog => "a backlog id like b41",
            Wanted::ListItem => "a chore or backlog id like c12 or b41",
            Wanted::Item => "an id like 128, c12 or b41",
        };
        write!(f, "`{}` is not {wanted}", self.word)
    }
}

impl std::error::Error for InvalidId {}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn list_id_grammar() {
        let c = ListKind::Chores;
        assert_eq!(c.parse_id(b"c12"), Some(12));
        assert_eq!(c.parse_id(b"c4294967295"), Some(u32::MAX));
        for not_an_id in [
            "",
            "c",
            "c0",
            "c012",
            "b12",
            "C12",
            "12",
            "c12.",
            "c1 2",
            "c+1",
            "c-1",
            "cc1",
            "c4294967296",
            "c99999999999999999999",
        ] {
            assert_eq!(c.parse_id(not_an_id.as_bytes()), None, "{not_an_id}");
        }
        assert_eq!(ListKind::Backlog.parse_id(b"b41"), Some(41));
        assert_eq!(ListKind::Backlog.parse_id(b"c41"), None);
    }

    #[test]
    fn brief_number_grammar() {
        assert_eq!("128".parse(), Ok(BriefId(128)));
        assert_eq!("007".parse(), Ok(BriefId(7)));
        assert_eq!("0".parse(), Ok(BriefId(0)));
        assert_eq!("4294967295".parse(), Ok(BriefId(u32::MAX)));
        for not_a_number in ["", "+1", "-1", "1a", " 1", "4294967296", "c1"] {
            assert!(not_a_number.parse::<BriefId>().is_err(), "{not_a_number}");
        }
    }

    #[test]
    fn an_argument_of_the_wrong_kind_is_refused_with_an_example() {
        assert_eq!(
            "b41".parse::<ChoreId>().unwrap_err().to_string(),
            "`b41` is not a chore id like c12"
        );
        assert_eq!(
            "c12".parse::<BacklogId>().unwrap_err().to_string(),
            "`c12` is not a backlog id like b41"
        );
        assert_eq!(
            "x".parse::<BriefId>().unwrap_err().to_string(),
            "`x` is not a brief number like 128"
        );
        assert_eq!(
            "c012".parse::<ItemId>().unwrap_err().to_string(),
            "`c012` is not an id like 128, c12 or b41"
        );
        assert_eq!(
            "128".parse::<ListId>().unwrap_err().to_string(),
            "`128` is not a chore or backlog id like c12 or b41"
        );
    }

    #[test]
    fn a_list_id_is_a_chore_or_a_backlog_id() {
        let c12 = "c12".parse::<ListId>().unwrap();
        let b41 = "b41".parse::<ListId>().unwrap();
        assert_eq!((c12.kind(), c12.get()), (ListKind::Chores, 12));
        assert_eq!((b41.kind(), b41.get()), (ListKind::Backlog, 41));
        assert_eq!(
            (c12.to_string(), b41.to_string()),
            ("c12".into(), "b41".into())
        );
        for not_one in ["", "12", "c0", "c012", "x1"] {
            assert!(not_one.parse::<ListId>().is_err(), "{not_one}");
        }
    }

    #[test]
    fn zero_is_not_a_list_id() {
        assert_eq!(ChoreId::new(0), None);
        assert_eq!(BacklogId::new(0), None);
        assert_eq!(ChoreId::new(1).map(ChoreId::get), Some(1));
    }

    fn any_item_id() -> impl Strategy<Value = ItemId> {
        prop_oneof![
            any::<u32>().prop_map(|n| ItemId::Brief(BriefId::new(n))),
            (1..=u32::MAX).prop_map(|n| ItemId::Chore(ChoreId::new(n).unwrap())),
            (1..=u32::MAX).prop_map(|n| ItemId::Backlog(BacklogId::new(n).unwrap())),
        ]
    }

    proptest! {
        #[test]
        fn an_id_reads_back_as_printed(id in any_item_id()) {
            prop_assert_eq!(id.to_string().parse(), Ok(id));
            match id {
                ItemId::Brief(_) => prop_assert!(id.to_string().parse::<ListId>().is_err()),
                ItemId::Chore(chore) => {
                    prop_assert_eq!(id.to_string().parse(), Ok(ListId::Chore(chore)));
                }
                ItemId::Backlog(backlog) => {
                    prop_assert_eq!(id.to_string().parse(), Ok(ListId::Backlog(backlog)));
                }
            }
        }

        /// One spelling per list id, so a search for the printed id finds
        /// every line that carries it. A brief number may have leading zeros.
        #[test]
        fn a_word_that_parses_is_the_printed_id(
            word in prop_oneof!["[bc0-9+ -]{0,12}", "[bc]?0{0,2}[0-9]{1,11}"],
        ) {
            if let Ok(id) = word.parse::<ItemId>() {
                match id {
                    ItemId::Brief(number) => {
                        prop_assert!(word.bytes().all(|b| b.is_ascii_digit()));
                        prop_assert_eq!(word.parse::<u64>().ok(), Some(u64::from(number.get())));
                    }
                    ItemId::Chore(_) | ItemId::Backlog(_) => prop_assert_eq!(id.to_string(), word),
                }
            }
        }
    }
}
