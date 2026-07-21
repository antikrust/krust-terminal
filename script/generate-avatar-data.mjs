// Extract per-style DiceBear generation data so a Rust engine can reproduce it.
//
// For each style we emit: viewBox/attributes, the ordered component groups
// (variants, probability, and each variant's SVG template with {{color:X}} /
// {{component:G}} placeholders), the color groups (pick lists), and the body
// layout (the <g transform> wrapping, with {{group:G}} markers). We also render
// a few reference avatars with the real library so the Rust output can be
// diffed against ground truth.

import { createAvatar } from '@dicebear/core';
import * as collection from '@dicebear/collection';
import { readFileSync } from 'fs';

const camel = (s) => s.replace(/-([a-z])/g, (_, c) => c.toUpperCase());

// A Proxy that yields a placeholder for any color key, and component objects
// whose .value() yields a {{component:group}} marker, so calling a component
// function returns its SVG template with our placeholders in place.
function colorProxy() {
  return new Proxy({}, { get: (_t, key) => `{{color:${String(key)}}}` });
}
function componentProxy() {
  return new Proxy(
    {},
    {
      get: (_t, group) => ({
        name: `{{compname:${String(group)}}}`,
        value: () => `{{component:${String(group)}}}`,
      }),
    }
  );
}

// Read the style's internal component collection (public API doesn't expose it;
// a direct file:// import bypasses the package "exports" map).
async function loadComponents(styleId) {
  const url = new URL(
    `./node_modules/@dicebear/${styleId}/lib/components/index.js`,
    import.meta.url
  ).href;
  return await import(url);
}

// Parse the create() body template out of the compiled index.js: the ordered
// sequence of <g ...>${... components.GROUP ...}</g> (or bare ${...GROUP...}).
//
// Interpolations are found with a balanced scanner, not a lazy regex: bodies
// like personas' `fill="${escape.xml(`${colors.skin}`)}"` nest template
// literals inside the interpolation, and a `[^]*?` match stops at the first
// `}` and corrupts the template.
function extractLayout(styleId) {
  const src = readFileSync(
    new URL(`./node_modules/@dicebear/${styleId}/lib/index.js`, import.meta.url)
  ).toString();
  // The body is a single template literal assigned to `body:`.
  const m = src.match(/body:\s*`([\s\S]*?)`,\s*\n\s*extra/);
  if (!m) return null;
  const body = processTemplate(m[1]);
  if (/[`]|\$\{/.test(body)) {
    throw new Error(`${styleId}: layout extraction left template artifacts`);
  }
  return body;
}

// Rewrite every ${...} interpolation in a template-literal body into our
// markers, scanning braces/backticks in a balanced way.
function processTemplate(tpl) {
  let out = '';
  let i = 0;
  while (i < tpl.length) {
    const d = tpl.indexOf('${', i);
    if (d === -1) {
      out += tpl.slice(i);
      break;
    }
    out += tpl.slice(i, d);
    let j = d + 2;
    let depth = 1;
    let expr = '';
    while (j < tpl.length && depth > 0) {
      const ch = tpl[j];
      if (ch === '`') {
        // Copy a whole nested template literal (including its own ${...}s,
        // themselves balanced) into the expression verbatim.
        expr += '`';
        j++;
        while (j < tpl.length && tpl[j] !== '`') {
          if (tpl[j] === '\\') {
            expr += tpl[j] + (tpl[j + 1] ?? '');
            j += 2;
            continue;
          }
          if (tpl[j] === '$' && tpl[j + 1] === '{') {
            let d2 = 1;
            expr += '${';
            j += 2;
            while (j < tpl.length && d2 > 0) {
              if (tpl[j] === '{') d2++;
              else if (tpl[j] === '}') d2--;
              if (d2 > 0) expr += tpl[j];
              j++;
            }
            expr += '}';
            continue;
          }
          expr += tpl[j];
          j++;
        }
        expr += '`';
        j++; // past the closing backtick
        continue;
      }
      if (ch === '{') depth++;
      else if (ch === '}') {
        depth--;
        if (depth === 0) {
          j++;
          break;
        }
      }
      if (depth > 0) expr += ch;
      j++;
    }
    out += classifyExpr(expr);
    i = j;
  }
  return out;
}

// Turn one interpolation expression into markers. Nested template literals
// (visibility ternaries, escape.xml(`...`) wrappers) contribute their
// processed contents; the falsy ternary branch is the empty literal in every
// style we bundle, so joining all branches is safe. A bare expression maps to
// its component/color reference, or disappears.
function classifyExpr(expr) {
  const nested = [];
  let k = 0;
  while (k < expr.length) {
    if (expr[k] === '`') {
      let body = '';
      let depth = 0;
      k++;
      while (k < expr.length && !(expr[k] === '`' && depth === 0)) {
        if (expr[k] === '$' && expr[k + 1] === '{') depth++;
        else if (expr[k] === '}' && depth > 0) depth--;
        body += expr[k];
        k++;
      }
      nested.push(body);
    }
    k++;
  }
  if (nested.length) return nested.map(processTemplate).join('');
  const g = expr.match(/components\.(\w+)/);
  if (g) return `{{group:${g[1]}}}`;
  const c = expr.match(/colors\.(\w+)/);
  if (c) return `{{color:${c[1]}}}`;
  return '';
}

// The bundled styles: the full upstream collection, nothing culled. Styles
// keep exactly the backgrounds their official defaults paint — none are
// invented here.
const REGISTRY = [
  ['bottts', 'Robots'], ['adventurer', 'Adventurer'], ['avataaars', 'Avataaars'],
  ['big-ears', 'Big Ears'], ['big-smile', 'Big Smile'], ['croodles', 'Croodles'],
  ['fun-emoji', 'Fun Emoji'], ['lorelei', 'Lorelei'], ['micah', 'Micah'],
  ['miniavs', 'Miniavs'], ['notionists', 'Notionists'], ['open-peeps', 'Open Peeps'],
  ['personas', 'Personas'], ['pixel-art', 'Pixel Art'], ['thumbs', 'Thumbs'],
  ['identicon', 'Identicon'], ['shapes', 'Shapes'], ['rings', 'Rings'],
];

async function extractStyle(styleId) {
  const style = collection[camel(styleId)];
  const schema = style.schema.properties ?? {};
  const comps = await loadComponents(styleId);

  // Component groups = keys present in the component collection.
  const groupNames = Object.keys(comps).filter((k) => k !== 'default');

  const groups = groupNames.map((g) => {
    const variants = schema[g]?.default ?? schema[g]?.items?.enum ?? [];
    const probability = schema[`${g}Probability`]?.default ?? null;
    const templates = {};
    for (const [variant, fn] of Object.entries(comps[g] ?? {})) {
      try {
        templates[variant] = fn(componentProxy(), colorProxy());
      } catch (e) {
        templates[variant] = `<!-- extract failed: ${e.message} -->`;
      }
    }
    return { name: g, variants, probability, templates };
  });

  // Color groups: instrument the real getColors so the keys match the template
  // placeholders exactly (e.g. clothesColor -> `clothes`) and we capture the
  // pick list per key. Raw values; convertColor (# prefix) happens in Rust.
  const resolvedOptions = {};
  for (const [k, v] of Object.entries(schema)) resolvedOptions[k] = v.default;
  const getColorsMod = await import(
    new URL(`./node_modules/@dicebear/${styleId}/lib/utils/getColors.js`, import.meta.url).href
  );
  const pickedArrays = [];
  const instrPrng = {
    pick: (arr, fallback) => {
      pickedArrays.push(Array.isArray(arr) ? arr : []);
      return (Array.isArray(arr) && arr[0]) ?? fallback ?? 'transparent';
    },
    integer: () => 0,
    bool: () => true,
    next: () => 0,
    shuffle: (a) => a,
    string: () => 'x',
  };
  let colors = [];
  try {
    const colorsObj = getColorsMod.getColors({ prng: instrPrng, options: resolvedOptions });
    colors = Object.keys(colorsObj).map((name, i) => ({ name, values: pickedArrays[i] ?? [] }));
  } catch {
    colors = [];
  }

  // Root attributes come from a real render (cheap, deterministic).
  const sample = createAvatar(style, { seed: 'sample' }).toString();
  const viewBox = sample.match(/viewBox="([^"]*)"/)?.[1] ?? '0 0 100 100';

  // Background palette: only for styles whose DEFAULT render actually paints
  // one. The schema default alone is not the truth — a style hook may disable
  // it (avataaars' onPostCreate clears its own legacy default, for example),
  // so probe a real render and trust what it did.
  const probe = createAvatar(style, { seed: 'probe' }).toJson();
  const paintsBackground = probe.extra.primaryBackgroundColor !== 'transparent';
  const background = paintsBackground ? schema.backgroundColor?.default ?? [] : [];

  return {
    id: styleId,
    viewBox,
    groups,
    colors,
    ...(background.length ? { background } : {}),
    layout: extractLayout(styleId),
    // Reference renders for diffing the Rust engine.
    reference: Object.fromEntries(
      ['01', '02', '03'].map((s) => [s, createAvatar(style, { seed: s }).toString()])
    ),
  };
}

import { mkdirSync, writeFileSync } from 'fs';
const OUT = new URL('./out/', import.meta.url);
mkdirSync(OUT, { recursive: true });

const combined = { styles: [], data: {}, reference: {} };
const summary = [];
for (const [id, label] of REGISTRY) {
  try {
    const data = await extractStyle(id);
    const { reference, ...bundled } = data;
    combined.styles.push({ id, label });
    combined.data[id] = bundled;
    combined.reference[id] = reference;
    summary.push(`${id}: ok (${data.groups.length} groups, ${data.colors.length} colors)`);
  } catch (e) {
    summary.push(`${id}: FAILED ${e.message}`);
  }
}
// The engine's data (no reference renders).
writeFileSync(new URL('avatars.json', OUT), JSON.stringify({ styles: combined.styles, data: combined.data }));
// Reference renders, kept separate for diffing the Rust engine.
writeFileSync(new URL('reference.json', OUT), JSON.stringify(combined.reference));
console.log(summary.join('\n'));
console.log('avatars.json bytes:', JSON.stringify({ styles: combined.styles, data: combined.data }).length);
