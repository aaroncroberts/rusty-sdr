#![forbid(unsafe_code)]

// Waterfall widget placeholder — scrolling spectrogram via GPU texture upload.
// Architecture: pre-allocated RGBA pixel buffer, shift rows on new FFT frame,
// upload via egui::TextureManager each render tick.
// TODO: implement
