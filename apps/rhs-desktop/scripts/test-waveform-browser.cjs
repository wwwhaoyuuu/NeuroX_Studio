const assert = require("node:assert/strict");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");

async function main() {
  const browser = await chromium.launch({ headless: true,
    ...(process.env.PLAYWRIGHT_BROWSER_PATH ? { executablePath: process.env.PLAYWRIGHT_BROWSER_PATH } : {}) });
  try {
    const page = await browser.newPage({ viewport: { width: 1920, height: 1080 } });
    page.setDefaultTimeout(10000);
    const errors = [];
    page.on("pageerror", (error) => errors.push(error.message));
    await page.goto(process.env.NEUROX_PREVIEW_URL || "http://127.0.0.1:1421/");
    assert.equal(await page.evaluate(async () => (await import("/bridge.js")).bridge.isPreview), true,
      "This test must never operate a hardware backend");
    await page.waitForFunction(() => document.querySelector("#mea-profile").options.length === 3);
    await page.selectOption("#mea-profile", "mea-ab-64");
    await page.waitForFunction(() => document.querySelectorAll(".channel-row").length === 64);
    for (const side of ["left", "right"]) {
      if (await page.locator(`#${side}-panel`).isVisible()) await page.click(`#toggle-${side}-panel`);
    }
    const left = await page.locator('.channel-label[data-channel="0"]').boundingBox();
    const right = await page.locator('.channel-label[data-channel="32"]').boundingBox();
    assert.ok(right.x > left.x + 500 && Math.abs(right.y - left.y) < 1);
    async function setRange(channel, min, max) {
      await page.selectOption("#focus-channel", String(channel));
      await page.fill("#focus-range-min", String(min));
      await page.fill("#focus-range-max", String(max));
      await page.click('#focus-range-apply');
    }
    await setRange(9, -20, 20);
    await setRange(41, -50, 80);
    await page.locator('.channel-label[data-channel="9"]').click();
    assert.equal(await page.locator("#waveform-single").getAttribute("aria-pressed"), "true");
    assert.equal(await page.inputValue("#focus-range-min"), "-20");
    assert.equal(await page.inputValue("#focus-range-max"), "20");
    await page.click("#waveform-multi");
    const canvas = await page.locator("#waveform").boundingBox();
    await page.mouse.click(canvas.x + canvas.width * 0.8, canvas.y + canvas.height * 9.5 / 32);
    assert.equal(await page.inputValue("#focus-channel"), "41");
    assert.equal(await page.inputValue("#focus-range-max"), "80");
    await page.click("#waveform-multi");
    await page.waitForTimeout(400);
    await page.reload();
    await page.waitForFunction(() => document.querySelectorAll(".channel-row").length === 64);
    await page.selectOption("#focus-channel", "9");
    assert.equal(await page.inputValue("#focus-range-max"), "20");
    await page.fill("#focus-range-max", "-30");
    await page.click('#focus-range-apply');
    assert.equal(await page.locator("#focus-range-max").evaluate((input) => input.validity.valid), false);
    await page.selectOption("#focus-channel", "41");
    assert.equal(await page.inputValue("#focus-range-max"), "80");
    assert.equal(await page.locator("#focus-range-max").evaluate((input) => input.validity.valid), true);
    await page.click("#focus-range-reset");
    assert.notEqual(await page.inputValue("#focus-range-max"), "80");
    const directory = path.resolve(__dirname, "../artifacts/dual-waveform");
    fs.mkdirSync(directory, { recursive: true });
    for (const [name, width, height] of [["desktop", 1920, 1080], ["laptop", 1460, 920], ["mobile", 390, 844]]) {
      await page.setViewportSize({ width, height });
      await page.waitForTimeout(200);
      assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
      assert.ok(await page.locator(".channel-row").first().evaluate((row) => row.getBoundingClientRect().height >= 15.9));
      const colored = await page.locator("#waveform").evaluate((canvas) => {
        const { data } = canvas.getContext("2d").getImageData(0, 0, canvas.width, canvas.height);
        let count = 0;
        for (let i = 0; i < data.length; i += 4) {
          if (Math.max(data[i], data[i + 1], data[i + 2]) - Math.min(data[i], data[i + 1], data[i + 2]) > 40) count++;
        }
        return count;
      });
      assert.ok(colored > 1000, `${name} canvas must contain waveforms`);
      await page.screenshot({ path: path.join(directory, `${name}.png`) });
    }
    await page.selectOption("#waveform-layout", "single-16");
    assert.equal(await page.locator(".channel-row").count(), 16);
    await page.click("#next-channel-page");
    assert.equal(await page.locator(".channel-label").first().getAttribute("data-channel"), "16");
    await page.selectOption("#waveform-layout", "dual-64");
    assert.equal(await page.locator(".channel-row").count(), 64);
    await page.setViewportSize({ width: 1460, height: 920 });
    if (!await page.locator("#left-panel").isVisible()) await page.click("#toggle-left-panel");
    async function savedRanges(profileId) {
      await page.waitForTimeout(400);
      return page.evaluate((id) => JSON.parse(localStorage.getItem("rhs-studio.workspace-recovery.v1"))
        .workspace.display.channelRanges[id] || {}, profileId);
    }
    for (const [profileId, count] of [["mea-ab-64", 64], ["rhs2116-logic-16", 16], ["mea-abcd-120", 120]]) {
      await page.selectOption("#mea-profile", profileId);
      await page.fill("#focus-range-min", "-20");
      await page.fill("#focus-range-max", "20");
      await page.click("#all-channel-range-apply");
      const ranges = await savedRanges(profileId);
      assert.equal(Object.keys(ranges).length, count, "bulk range must include every page, not just visible channels");
      assert.ok(Object.values(ranges).every((range) => range.minimumUv === -20 && range.maximumUv === 20));
    }
    await page.selectOption("#mea-profile", "mea-ab-64");
    await setRange(9, -10, 50);
    const individual = await savedRanges("mea-ab-64");
    assert.deepEqual(individual["B-009"], { minimumUv: -10, maximumUv: 50 });
    assert.deepEqual(individual["C-009"], { minimumUv: -20, maximumUv: 20 });
    await page.fill("#focus-range-min", "30");
    await page.fill("#focus-range-max", "20");
    await page.click("#all-channel-range-apply");
    assert.deepEqual(await savedRanges("mea-ab-64"), individual, "invalid bulk input must not overwrite saved ranges");
    await page.fill("#focus-range-min", "-20");
    await page.click("#all-channel-range-apply");
    await page.waitForTimeout(400);
    await page.reload();
    await page.waitForFunction(() => document.querySelectorAll(".channel-row").length === 64);
    await page.selectOption("#focus-channel", "63");
    assert.equal(await page.inputValue("#focus-range-min"), "-20");
    assert.equal(await page.inputValue("#focus-range-max"), "20");
    await page.locator('.channel-label[data-channel="63"]').click();
    assert.equal(await page.inputValue("#focus-range-max"), "20", "bulk limits also apply to channel detail");
    await page.click("#all-channel-range-reset");
    assert.deepEqual(await savedRanges("mea-ab-64"), {});
    assert.equal(Object.keys(await savedRanges("mea-abcd-120")).length, 120, "bulk reset must not change another source profile");
    assert.deepEqual(errors, []);
    console.log("PASS: synthetic 32+32 layout, individual/bulk ranges for 16/64/120 channels, validation/recovery/reset isolation, screenshots and nonblank canvas");
  } finally { await browser.close(); }
}

main().catch((error) => { console.error(error); process.exitCode = 1; });
