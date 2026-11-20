use crate::RenderError;
use peersus_api::canvas::{FrameHeader, FrameLayout, MAX_CANVAS_SCALE, display as dl, valid_id};

/// A bounded display list with a checked begin command. The renderer validates the body
/// incrementally, borrowing it from guest memory until the frame has been completed.
#[derive(Clone, Copy, Debug)]
pub struct DisplayList<'a> {
    pub(crate) header: FrameHeader<'a>,
    pub(crate) handle: i32,
    pub(crate) scale: u32,
    pub(crate) palette: [u8; dl::PALETTE_BYTES],
    pub(crate) commands: Reader<'a>,
}

impl<'a> DisplayList<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self, RenderError> {
        if bytes.len() > dl::MAX_DISPLAY_LIST_BYTES {
            return Err(RenderError::Limit);
        }
        let mut commands = Reader(bytes);
        if commands.array::<4>()? != dl::MAGIC || commands.u8()? != dl::BEGIN {
            return Err(RenderError::Invalid);
        }
        let id_len = usize::from(commands.u8()?);
        let id = commands.utf8(id_len)?;
        let handle = i32::from_le_bytes(commands.array()?);
        let seq = u64::from_le_bytes(commands.array()?);
        let settled = u64::from_le_bytes(commands.array()?);
        if !valid_id(id) || handle <= 0 || seq == 0 {
            return Err(RenderError::Invalid);
        }
        let layout = FrameLayout::tight(commands.u32()?, commands.u32()?)?;
        let scale = commands.u32()?;
        if !(1..=MAX_CANVAS_SCALE).contains(&scale) {
            return Err(RenderError::Invalid);
        }
        let palette = commands.array()?;
        Ok(Self {
            header: FrameHeader {
                id,
                layout,
                seq,
                settled,
            },
            handle,
            scale,
            palette,
            commands,
        })
    }

    pub fn header(&self) -> FrameHeader<'a> {
        self.header
    }

    pub fn handle(&self) -> i32 {
        self.handle
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    pub(crate) fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Result<&'a [u8], RenderError> {
        let (field, rest) = self.0.split_at_checked(len).ok_or(RenderError::Invalid)?;
        self.0 = rest;
        Ok(field)
    }

    pub(crate) fn array<const N: usize>(&mut self) -> Result<[u8; N], RenderError> {
        let (field, rest) = self
            .0
            .split_first_chunk::<N>()
            .ok_or(RenderError::Invalid)?;
        self.0 = rest;
        Ok(*field)
    }

    pub(crate) fn utf8(&mut self, len: usize) -> Result<&'a str, RenderError> {
        std::str::from_utf8(self.bytes(len)?).map_err(|_| RenderError::Invalid)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, RenderError> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, RenderError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, RenderError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn number(&mut self) -> Result<f32, RenderError> {
        let x = f32::from_le_bytes(self.array()?);
        if x.abs() <= dl::MAX_COORDINATE {
            Ok(x)
        } else {
            Err(RenderError::Invalid)
        }
    }
}
