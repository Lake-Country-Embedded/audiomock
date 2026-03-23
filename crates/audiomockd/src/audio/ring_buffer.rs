use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// A lock-free single-producer single-consumer ring buffer for f32 audio samples.
#[derive(Debug)]
pub struct RingBuffer {
    buffer: Vec<f32>,
    capacity: usize,
    read_pos: AtomicUsize,
    write_pos: AtomicUsize,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Arc<Self> {
        Arc::new(Self {
            buffer: vec![0.0; capacity],
            capacity,
            read_pos: AtomicUsize::new(0),
            write_pos: AtomicUsize::new(0),
        })
    }

    /// Number of samples available to read.
    pub fn available(&self) -> usize {
        let w = self.write_pos.load(Ordering::Acquire);
        let r = self.read_pos.load(Ordering::Acquire);
        if w >= r {
            w - r
        } else {
            self.capacity - r + w
        }
    }

    /// Number of free slots for writing.
    pub fn free(&self) -> usize {
        self.capacity - 1 - self.available()
    }

    /// Write samples into the ring buffer. Returns number of samples actually written.
    pub fn write(&self, data: &[f32]) -> usize {
        let free = self.free();
        let to_write = data.len().min(free);
        let mut w = self.write_pos.load(Ordering::Relaxed);

        let buf_ptr = self.buffer.as_ptr() as *mut f32;

        for i in 0..to_write {
            unsafe {
                *buf_ptr.add(w) = data[i];
            }
            w += 1;
            if w >= self.capacity {
                w = 0;
            }
        }

        self.write_pos.store(w, Ordering::Release);
        to_write
    }

    /// Read samples from the ring buffer. Returns number of samples actually read.
    pub fn read(&self, output: &mut [f32]) -> usize {
        let avail = self.available();
        let to_read = output.len().min(avail);
        let mut r = self.read_pos.load(Ordering::Relaxed);

        for i in 0..to_read {
            output[i] = self.buffer[r];
            r += 1;
            if r >= self.capacity {
                r = 0;
            }
        }

        self.read_pos.store(r, Ordering::Release);
        to_read
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basic_write_read() {
        let rb = RingBuffer::new(16);
        let data = [1.0f32, 2.0, 3.0, 4.0];
        assert_eq!(rb.write(&data), 4);
        assert_eq!(rb.available(), 4);

        let mut out = [0.0f32; 4];
        assert_eq!(rb.read(&mut out), 4);
        assert_eq!(out, data);
        assert_eq!(rb.available(), 0);
    }

    #[test]
    fn wrap_around() {
        let rb = RingBuffer::new(8);
        let data = [1.0f32; 6];
        assert_eq!(rb.write(&data), 6);

        let mut out = [0.0f32; 4];
        assert_eq!(rb.read(&mut out), 4);

        let data2 = [2.0f32; 5];
        assert_eq!(rb.write(&data2), 5);
        assert_eq!(rb.available(), 7); // 2 remaining + 5 new
    }
}
