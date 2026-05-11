#!/usr/bin/env node

const fs = require('fs');
const path = require('path');
const { execSync } = require('child_process');

const binDir = path.join(__dirname, 'bin');
const targetBinary = path.join(__dirname, 'target', 'release', 'crig');

// Create bin directory if it doesn't exist
if (!fs.existsSync(binDir)) {
  fs.mkdirSync(binDir, { recursive: true });
}

// Check if binary exists, if not build it
if (!fs.existsSync(targetBinary)) {
  console.log('Building crig binary...');
  try {
    execSync('cargo build --release', { stdio: 'inherit', cwd: __dirname });
  } catch (error) {
    console.error('Failed to build crig. Please make sure Rust is installed.');
    process.exit(1);
  }
}

// Copy binary to bin directory
const destBinary = path.join(binDir, 'crig');
fs.copyFileSync(targetBinary, destBinary);
fs.chmodSync(destBinary, '755');

console.log('crig installed successfully!');
