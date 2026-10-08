use crate::*;

pub(crate) fn parse_template(path: &str) -> Vec<Segment> {
    split_path(path)
        .into_iter()
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                assert!(part.len() > 2, "invalid route template");
                Segment::Capture(part[1..part.len() - 1].to_owned())
            } else {
                assert!(
                    !part.contains('{') && !part.contains('}'),
                    "invalid route template"
                );
                Segment::Static(part.to_owned())
            }
        })
        .collect()
}

pub(crate) fn split_path(path: &str) -> Vec<&str> {
    PathParts::new(path).map(|part| part.value).collect()
}

#[derive(Clone, Copy)]
pub(crate) struct PathPart<'a> {
    pub(crate) value: &'a str,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

#[derive(Clone, Copy)]
pub(crate) struct PathParts<'a> {
    pub(crate) path: &'a str,
    pub(crate) next: usize,
}

impl<'a> PathParts<'a> {
    pub(crate) fn new(path: &'a str) -> Self {
        Self { path, next: 0 }
    }
}

impl<'a> Iterator for PathParts<'a> {
    type Item = PathPart<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let bytes = self.path.as_bytes();
        while self.next < bytes.len() && bytes[self.next] == b'/' {
            self.next += 1;
        }
        if self.next >= bytes.len() {
            return None;
        }
        let start = self.next;
        while self.next < bytes.len() && bytes[self.next] != b'/' {
            self.next += 1;
        }
        Some(PathPart {
            value: &self.path[start..self.next],
            start,
            end: self.next,
        })
    }
}

pub(crate) fn normalize_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_owned()
    } else if path.len() > 1 {
        path.trim_end_matches('/').to_owned()
    } else {
        path.to_owned()
    }
}

pub(crate) fn normalize_request_path(path: &str) -> &str {
    if path.len() > 1 {
        path.trim_end_matches('/')
    } else {
        path
    }
}
