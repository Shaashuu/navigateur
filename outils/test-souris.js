// Test de l'interface (assets/chrome.html) dans Chromium sans écran : la barre
// au survol et le trait bleu de la poignée se referment quand la souris passe
// sur la page, même si l'interface ne reçoit plus aucun événement de souris
// (cas de macOS : seule la position lue par Rust, navigateur.pointer, arrive).
//
// Lancement : node outils/test-souris.js  (Playwright installé)
const assert = require('assert');
const path = require('path');
const { chromium } = require('playwright');

const CHROME = 'file://' + path.resolve(__dirname, '../assets/chrome.html');

async function page(browser, style) {
  const p = await browser.newPage({ viewport: { width: 1200, height: 800 } });
  await p.addInitScript(() => {
    window.sent = [];
    window.ipc = { postMessage: (m) => window.sent.push(JSON.parse(m)) };
  });
  await p.goto(CHROME);
  await p.evaluate((style) => navigateur.setState({
    tabs: [{ id: 1, title: 'A', url: 'https://a.fr', active: true }, { id: 2, title: 'B', url: 'https://b.fr' }],
    overlay: true,
    settings: { bar_position: 'left', sidebar_style: style, sidebar_width: 240 },
  }), style);
  return p;
}

async function poignee(browser) {
  const p = await page(browser, 'palette');
  const r = await p.evaluate(() => {
    const r = document.getElementById('resizer').getBoundingClientRect();
    return { x: r.x + r.width / 2 };
  });
  const trait = () => p.evaluate(() => Number(getComputedStyle(document.getElementById('resizer'), '::after').opacity));
  const watch = () => p.evaluate(() => sent.filter((m) => m.type === 'watch_pointer').map((m) => m.on));

  await p.mouse.move(r.x, 300);
  await p.waitForTimeout(300);
  assert.ok(await trait() > 0.5, 'trait allumé sur la poignée');
  assert.deepStrictEqual(await watch(), [true], 'Rust lit la souris');

  // La souris passe sur la page : aucun événement (macOS), le trait reste.
  await p.waitForTimeout(400);
  assert.ok(await trait() > 0.5);
  // Position donnée par Rust : sur la page.
  await p.evaluate(() => navigateur.pointer(800, 300));
  await p.waitForTimeout(300);
  assert.strictEqual(await trait(), 0, 'trait éteint quand la souris est sur la page');
  assert.deepStrictEqual(await watch(), [true, false], 'Rust arrête de lire la souris');

  // Retour sur la poignée : pour WebKit, la souris ne l'avait jamais quittée.
  await p.mouse.move(r.x, 400);
  await p.waitForTimeout(300);
  assert.ok(await trait() > 0.5, 'trait rallumé au retour');
  await p.evaluate(() => navigateur.pointer(null, null));
  await p.waitForTimeout(300);
  assert.strictEqual(await trait(), 0, 'trait éteint hors de la fenêtre');
  await p.close();
}

async function survol(browser) {
  const p = await page(browser, 'hover');
  const ouverte = () => p.evaluate(() => document.body.classList.contains('hovering'));
  await p.mouse.move(40, 300);
  await p.mouse.move(41, 310);
  await p.waitForTimeout(400);
  assert.ok(await ouverte(), 'barre ouverte au survol');
  await p.waitForTimeout(600);
  assert.ok(await ouverte(), 'sans événement, elle reste ouverte');
  await p.evaluate(() => navigateur.pointer(20, 300));
  await p.waitForTimeout(600);
  assert.ok(await ouverte(), 'souris sur la barre : elle reste');
  await p.evaluate(() => navigateur.pointer(800, 300));
  await p.waitForTimeout(700);
  assert.ok(!await ouverte(), 'souris sur la page : elle se referme');
  await p.close();
}

(async () => {
  const browser = await chromium.launch();
  try {
    await poignee(browser);
    console.log('poignée : ok');
    await survol(browser);
    console.log('barre au survol : ok');
  } finally {
    await browser.close();
  }
})().catch((err) => { console.error(err); process.exit(1); });
