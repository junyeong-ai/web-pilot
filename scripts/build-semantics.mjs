import { build } from 'esbuild';
import { roles } from 'aria-query';
import { readFile, writeFile } from 'node:fs/promises';

const concreteRoles = [...roles].filter(([, role]) => !role.abstract).map(([name]) => name);
const result = await build({
  stdin: {
    contents: `export { computeAccessibleName, computeAccessibleDescription, getRole } from 'dom-accessibility-api';\nexport const concreteRoles = new Set(${JSON.stringify(concreteRoles)});`,
    resolveDir: process.cwd(),
  },
  bundle: true,
  format: 'iife',
  globalName: 'WebPilotSemantics',
  target: 'chrome120',
  minify: true,
  legalComments: 'none',
  write: false,
});
const license = await readFile('node_modules/dom-accessibility-api/LICENSE.md', 'utf8');
const ariaLicense = await readFile('node_modules/aria-query/LICENSE', 'utf8');
const output = result.outputFiles[0].text;
if (process.argv.includes('--check')) {
  if (output !== await readFile('extension/vendor/semantics.js', 'utf8') ||
      license !== await readFile('extension/vendor/LICENSE.dom-accessibility-api', 'utf8') ||
      ariaLicense !== await readFile('extension/vendor/LICENSE.aria-query', 'utf8')) {
    throw new Error('Run npm run build to update browser semantics');
  }
} else {
  await writeFile('extension/vendor/semantics.js', output);
  await writeFile('extension/vendor/LICENSE.dom-accessibility-api', license);
  await writeFile('extension/vendor/LICENSE.aria-query', ariaLicense);
}
