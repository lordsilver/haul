//! Enums whose every variant has one name, said once: the name serde reads
//! and writes, what `as_str` and `Display` give, and what `FromStr` takes.

/// Defines `enum $name` with each variant `= "word"`, and its names. The
/// caller writes the derives (serde's among them) and any other attributes.
macro_rules! named {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident {
            $($(#[$vmeta:meta])* $variant:ident = $word:literal,)+
        }
    ) => {
        $(#[$meta])*
        $vis enum $name {
            $($(#[$vmeta])* #[serde(rename = $word)] $variant,)+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $word,)+
                }
            }
        }

        impl std::str::FromStr for $name {
            type Err = ();

            fn from_str(s: &str) -> Result<Self, ()> {
                match s {
                    $($word => Ok(Self::$variant),)+
                    _ => Err(()),
                }
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.as_str() == *other
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }
    };
}

pub(crate) use named;
