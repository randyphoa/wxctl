#!/usr/bin/env node
// Force the meta shim to see a linux/musl host, so the musl-awareness path runs
// on a non-musl dev host (macOS/glibc). The shim then resolves the -musl
// sub-package, finds nothing, exits cleanly, and never spawns the present -gnu
// binary. Usage: node e2e-force-libc.mjs <arch> <path-to-shim.mjs>
import { pathToFileURL } from 'node:url';

const arch = process.argv[2] || 'arm64';
const shim = process.argv[3];
Object.defineProperty(process, 'platform', { value: 'linux' });
Object.defineProperty(process, 'arch', { value: arch });
process.report.getReport = () => ({ header: {} }); // no glibcVersionRuntime -> musl
if (process.platform !== 'linux' || process.arch !== arch) {
  process.stderr.write('force-libc: could not override process.platform/arch\n');
  process.exit(2);
}
await import(pathToFileURL(shim).href);
