# Maintenance Notes

## Do Before Each Release

- Make sure you didn't forget any license notices: `rg -g '*.rs' --files-without-match -F 'GNU AGPL v3.0'`
- Make sure you didn't introduce any lint warnings: `cargo clippy`
