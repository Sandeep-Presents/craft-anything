use super::*;
pub(crate) struct Input<'a> {
    pub data: &'a [u8],
    pub pos: usize,
    pub len: usize,
}
impl<'a> Input<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, len: data.len() }
    }
    pub fn take<const N: usize>(&mut self) -> Result<[u8; N]> {
        let mut b = [0u8; N];
        if checked_add(self.pos, N)? > self.len {
            return Err(fail("truncated", "Geometry field missing"));
        }
        b.copy_from_slice(bytes(self.data, self.pos, N)?);
        self.pos = checked_add(self.pos, N)?;
        Ok(b)
    }
    pub fn seek(&mut self, pos: usize) -> Result<()> {
        if pos > self.len {
            return Err(fail("truncated", "Geometry range missing"));
        }
        self.pos = pos;
        Ok(())
    }
    pub fn skip(&mut self, n: usize) -> Result<()> {
        self.seek(checked_add(self.pos, n)?)
    }
    pub fn marker(&mut self, expected: &[u8]) -> Result<()> {
        for b in expected {
            if self.take::<1>()? != [*b] {
                return Err(Failure {
                    code: "unsupported_layout",
                    message: "Unexpected geometry marker".into(),
                    section: None,
                    offset: Some(self.pos.saturating_sub(1)),
                });
            }
        }
        Ok(())
    }
    pub fn byte(&mut self) -> Result<u8> {
        Ok(u8::from_le_bytes(self.take()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    pub fn count(&mut self, cap: usize) -> Result<usize> {
        let n = usize::try_from(self.u32()?).map_err(|_| fail("limit", "Geometry count"))?;
        if n > cap {
            return Err(fail("limit", "Geometry count budget"));
        }
        Ok(n)
    }
    pub fn f64(&mut self) -> Result<f64> {
        let v = f64::from_le_bytes(self.take()?);
        if !v.is_finite() {
            return Err(fail("unsupported_value", "Nonfinite geometry scalar"));
        }
        Ok(v)
    }
    pub fn f32(&mut self) -> Result<f64> {
        let v = f32::from_le_bytes(self.take()?);
        if !v.is_finite() {
            return Err(fail("unsupported_value", "Nonfinite geometry scalar"));
        }
        Ok(f64::from(v))
    }
    pub fn guid(&mut self) -> Result<String> {
        let raw = self.take::<16>()?;
        if raw.iter().all(|b| *b == 0) {
            return Err(fail("invalid_identity", "Nil geometry GUID"));
        }
        guid_at(&raw, 0)
    }
    pub fn string(&mut self) -> Result<String> {
        self.marker(&[255, 254, 255])?;
        let n = self.byte()?;
        let n = if n == 255 { usize::from(u16::from_le_bytes(self.take()?)) } else { usize::from(n) };
        if n > 16384 {
            return Err(fail("limit", "Geometry string budget"));
        }
        let mut units = Vec::with_capacity(n);
        for _ in 0..n {
            units.push(u16::from_le_bytes(self.take()?));
        }
        String::from_utf16(&units).map_err(|_| fail("invalid_string", "Geometry UTF16"))
    }
}
