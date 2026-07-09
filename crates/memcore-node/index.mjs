import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const binding = require('./index.js');

export const { JsMemoryStore, isNoise, shouldSkip } = binding;
export default binding;
