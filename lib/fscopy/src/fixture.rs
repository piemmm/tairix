//! What the copying tools' in-memory test filesystems share.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::Errno;

/// The hops a resolution takes before it is a cycle.
const MAX_HOPS: usize = 8;

/// `path` resolved as the kernel resolves it, in miniature: each component a
/// link (`link_of` answering its target) replaced by that target — absolute,
/// or relative to the link's directory — and each `..` climbing what was
/// walked.
///
/// # Errors
///
/// [`Errno::LinkLoop`] past eight links.
pub fn resolve(path: &str, link_of: impl Fn(&str) -> Option<String>) -> Result<String, Errno> {
    let mut walked: Vec<String> = Vec::new();
    let mut pending: Vec<String> = path.split('/').rev().map(String::from).collect();
    let mut hops = 0;
    while let Some(part) = pending.pop() {
        match part.as_str() {
            "" | "." => continue,
            ".." => {
                walked.pop();
                continue;
            }
            _ => {}
        }
        if let Some(target) = link_of(&spelled(walked.iter().chain([&part]))) {
            hops += 1;
            if hops > MAX_HOPS {
                return Err(Errno::LinkLoop);
            }
            if target.starts_with('/') {
                walked.clear();
            }
            pending.extend(target.split('/').rev().map(String::from));
            continue;
        }
        walked.push(part);
    }
    Ok(spelled(walked.iter()))
}

/// The absolute path `components` spell, `/` for none.
fn spelled<'a>(components: impl Iterator<Item = &'a String>) -> String {
    let mut path = String::new();
    for component in components {
        path.push('/');
        path.push_str(component);
    }
    if path.is_empty() {
        path.push('/');
    }
    path
}

#[cfg(test)]
mod tests {
    use alloc::string::String;

    use super::resolve;
    use tairix_abi::Errno;

    fn links(path: &str) -> Option<String> {
        match path {
            "/up" => Some(String::from("..")),
            "/a/rel" => Some(String::from("b/c")),
            "/abs" => Some(String::from("/a")),
            "/loop" => Some(String::from("/loop")),
            _ => None,
        }
    }

    #[test]
    fn each_link_and_dot_dot_on_the_way_is_resolved() {
        assert_eq!(resolve("/abs/rel/d", links), Ok(String::from("/a/b/c/d")));
        assert_eq!(resolve("/a/../up/x", links), Ok(String::from("/x")));
        assert_eq!(resolve("//a/./b/", links), Ok(String::from("/a/b")));
        assert_eq!(resolve("/..", links), Ok(String::from("/")));
    }

    #[test]
    fn a_cycle_of_links_is_cut_off() {
        assert_eq!(resolve("/loop/x", links), Err(Errno::LinkLoop));
    }
}
