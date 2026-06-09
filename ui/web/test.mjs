import { existsSync } from 'node:fs';

for (const file of ['src/index.html', 'src/app.js', 'src/styles.css']) {
  if (!existsSync(file)) {
    throw new Error(`${file} missing`);
  }
}

console.log('UI fixture files present');
