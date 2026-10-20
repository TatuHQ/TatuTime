import { cp, mkdir, readFile, writeFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';

const source = new URL('./node_modules/pdfjs-dist/', import.meta.url);
const target = new URL('../web/vendor/pdfjs/', import.meta.url);
const metadata = JSON.parse(await readFile(new URL('package.json', source), 'utf8'));
await mkdir(target, { recursive: true });
const hashes = {};
for (const name of ['pdf.mjs', 'pdf.worker.mjs']) {
  const bytes = await readFile(new URL(`build/${name}`, source));
  await writeFile(new URL(name, target), bytes);
  hashes[name] = createHash('sha256').update(bytes).digest('hex');
}
await cp(new URL('standard_fonts/', source), new URL('standard_fonts/', target), { recursive: true });
await cp(new URL('LICENSE', source), new URL('LICENSE', target));
await writeFile(new URL('manifest.json', target), JSON.stringify({
  package: 'pdfjs-dist', version: metadata.version, sha256: hashes,
}, null, 2) + '\n');
console.log(`Vendored PDF.js ${metadata.version}`);