def edit(p,a,b):
    s=open(p,encoding='utf-8',newline='').read()
    nl='\r\n' if '\r\n' in s else '\n'
    a=a.replace('\n',nl); b=b.replace('\n',nl)
    assert a in s,a
    open(p,'w',encoding='utf-8',newline='').write(s.replace(a,b,1))
p='src/multipart.rs'
edit(p,'''    inner: http_body_util::BodyDataStream<Incoming>,
    progress: Arc<Progress>,
}''','''    inner: http_body_util::BodyDataStream<Incoming>,
    progress: Arc<Progress>,
    /// `\r\n--boundary`: while a skipped part is drained, its end is the
    /// next delimiter, after which the guard applies again.
    delimiter: Vec<u8>,
    /// The last bytes seen while draining (a delimiter can straddle chunks).
    carry: Vec<u8>,
}''')
edit(p,'''                if self.progress.draining.load(Ordering::Relaxed) {
                    // Skipping a part: the whole-stream limit bounds this.
                    self.progress.unproductive.store(0, Ordering::Relaxed);
                    return Poll::Ready(Some(Ok(bytes)));
                }''','''                if self.progress.draining.load(Ordering::Relaxed) {
                    // Skipping a part: its data is not a violation, but only
                    // up to the delimiter that ends it. What follows (the next
                    // part's headers) counts as unproductive again.
                    let mut window = std::mem::take(&mut self.carry);
                    window.extend_from_slice(&bytes);
                    let found = window
                        .windows(self.delimiter.len())
                        .position(|candidate| candidate == self.delimiter.as_slice());
                    if let Some(position) = found {
                        let after = window.len() - position - self.delimiter.len();
                        self.progress.draining.store(false, Ordering::Relaxed);
                        self.progress
                            .unproductive
                            .store(after as u64, Ordering::Relaxed);
                    } else {
                        let keep = self.delimiter.len().saturating_sub(1).min(window.len());
                        self.carry = window.split_off(window.len() - keep);
                        self.progress.unproductive.store(0, Ordering::Relaxed);
                    }
                    return Poll::Ready(Some(Ok(bytes)));
                }''')
edit(p,'''        let stream = GuardedBody {
            inner: request.into_body().into_data_stream(),
            progress: Arc::clone(&progress),
        };''','''        let stream = GuardedBody {
            inner: request.into_body().into_data_stream(),
            progress: Arc::clone(&progress),
            delimiter: format!("\r\n--{boundary}").into_bytes(),
            carry: Vec::new(),
        };''')
