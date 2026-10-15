//! The app between starts (ARCH-03, PLATFORM-05): what quitting leaves
//! unwatched, and what is redone after the computer slept.
//!
//! | Module | What lives there |
//! | --- | --- |
//! | `quit` | what quitting now would leave unwatched, in the words the trader confirms, and the one-check-at-a-time gate the window's close goes through |

pub mod quit;
