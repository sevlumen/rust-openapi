use crate::*;

pub(crate) const MAX_CAPTURE_PARAMS: usize = 8;

#[derive(Clone, Copy, Debug)]
pub(crate) struct CaptureRange {
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// A path capture collection. Typed extractors use ranges into the request
/// URI, while the explicit `Params` extractor opts into owned decoded values.
#[derive(Debug)]
pub(crate) struct MaterializedParams {
    pub(crate) names: Arc<[String]>,
    pub(crate) values: Box<[String]>,
}

pub(crate) type OwnedParams = Arc<MaterializedParams>;

#[derive(Clone, Debug)]
pub struct Params {
    pub(crate) packed: [u64; MAX_CAPTURE_PARAMS],
    pub(crate) count: u8,
    pub(crate) owned: Option<OwnedParams>,
}

pub(crate) static EMPTY_PARAMS: Params = Params {
    packed: [0; MAX_CAPTURE_PARAMS],
    count: 0,
    owned: None,
};

impl Params {
    pub(crate) fn empty() -> &'static Self {
        &EMPTY_PARAMS
    }

    pub fn get(&self, name: &str) -> Option<&str> {
        let owned = self.owned.as_ref()?;
        owned
            .names
            .iter()
            .zip(owned.values.iter())
            .find(|(key, _)| key.as_str() == name)
            .map(|(_, value)| value.as_str())
    }

    pub(crate) fn first_raw<'a>(&self, path: &'a str) -> Option<&'a str> {
        self.range(0).map(|range| &path[range.start..range.end])
    }

    pub(crate) fn range(&self, index: usize) -> Option<CaptureRange> {
        (index < self.count as usize).then(|| {
            let packed = self.packed[index];
            CaptureRange {
                start: (packed >> 32) as usize,
                end: packed as u32 as usize,
            }
        })
    }

    pub(crate) fn from_match(
        names: &[String],
        captures: CaptureSet,
        path: &str,
        materialize: bool,
    ) -> Self {
        let owned = materialize.then(|| {
            Self::materialized_names(
                Arc::from(names.to_owned().into_boxed_slice()),
                &captures,
                path,
            )
            .expect("materialized capture values are valid")
        });
        Self {
            packed: captures.packed,
            count: captures.count,
            owned,
        }
    }

    pub(crate) fn from_materialized_names(
        names: Arc<[String]>,
        captures: CaptureSet,
        path: &str,
    ) -> Result<Self, ApiError> {
        Ok(Self {
            packed: captures.packed,
            count: captures.count,
            owned: Some(Self::materialized_names(names, &captures, path)?),
        })
    }

    pub(crate) fn materialized_names(
        names: Arc<[String]>,
        captures: &CaptureSet,
        path: &str,
    ) -> Result<OwnedParams, ApiError> {
        let mut values = Vec::with_capacity(captures.count as usize);
        for index in 0..captures.count as usize {
            let range = captures.range(index).expect("capture range exists");
            values.push(percent_decode(&path[range.start..range.end])?);
        }
        Ok(Arc::new(MaterializedParams {
            names,
            values: values.into_boxed_slice(),
        }))
    }
}

impl Default for Params {
    fn default() -> Self {
        Self {
            packed: [0; MAX_CAPTURE_PARAMS],
            count: 0,
            owned: None,
        }
    }
}

impl<S: Send + Sync + 'static> FromRequest<S> for Params {
    const NEEDS_PARAMS: bool = true;
    const NEEDS_CAPTURE: bool = true;

    fn from_request(
        _request: &mut Request<Bytes>,
        params: &Params,
        _state: &Arc<S>,
    ) -> Result<Self, ApiError> {
        Ok(params.clone())
    }
}
