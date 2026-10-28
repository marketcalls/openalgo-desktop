//! Web search for the Agent. Carried here only as its taint boundary
//! (`taint`), byte for byte from pr/agent-chatgpt-search, because the chart
//! tools build a drawing's caption from the trader's own words with it;
//! that lane's full module replaces this file at restack.

pub mod taint;

#[cfg(test)]
mod tests;
