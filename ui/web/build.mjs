import { cpSync, mkdirSync, rmSync } from 'node:fs';
import { resolve } from 'node:path';

const src = resolve('src');
const dist = resolve('dist');
rmSync(dist, { recursive: true, force: true });
mkdirSync(dist, { recursive: true });
cpSync(src, dist, { recursive: true });
console.log(`Built static web UI into ${dist}`);
