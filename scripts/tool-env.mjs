if (process.platform === 'win32') {
  const key = Object.keys(process.env).find(name => name.toLowerCase() === 'path');
  if (key) {
    // An unmatched quote in one Windows PATH entry can hide later tools from Rust.
    // Normalize this child process only; leave the machine and user PATH intact.
    process.env[key] = process.env[key].split(';').map(entry => entry.replace(/^"|"$/g, '')).join(';');
  }
}
