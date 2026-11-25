// The space scene behind the Home hero: GradMesh v5's station
// (components/three/station.ts + SpaceBackdrop.tsx), as a plain module.
//
// The model is models/space-station.glb, "space boi" by silvercrow101,
// CC BY-NC 4.0 (models/CREDITS.md). It has no animation clips; motion is built
// from its parts by node name: body bobs, waves* pulse, particles drift,
// Sphere* planets orbit, Cube (a ground slab) is hidden.
//
// Cost rules: one renderer for the life of the window, created on first use;
// it renders only while the hero is attached, on screen and the window
// visible; DPR is capped at 1.5; reduced motion holds it still; no WebGL means
// a CSS still (`.scene-fallback`) and three.js is never even loaded.

export const CREDIT_URL = "https://sketchfab.com/3d-models/space-boi-f6a8c6a6727b4f2cb020c8b50bb2ee60";
const MODEL_URL = "./models/space-station.glb";
const VENDOR_RINGS = [0xa3d65c, 0x4ea8ff, 0xd3d5de];

const SKIP = /^cube/i;
const BODY = /^body$/i;
const WAVES = /^waves\d*$/i;
const STARS = /^particles$/i;
const PLANET = /^sphere\d*$/i;

export function webglAvailable() {
  try {
    const canvas = document.createElement("canvas");
    return Boolean(canvas.getContext("webgl2") || canvas.getContext("webgl"));
  } catch {
    return false;
  }
}

function glowTexture(THREE) {
  const canvas = document.createElement("canvas");
  canvas.width = canvas.height = 128;
  const ctx = canvas.getContext("2d");
  const g = ctx.createRadialGradient(64, 64, 0, 64, 64, 64);
  g.addColorStop(0, "rgba(255,255,255,0.55)");
  g.addColorStop(0.35, "rgba(255,255,255,0.18)");
  g.addColorStop(1, "rgba(255,255,255,0)");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, 128, 128);
  return new THREE.CanvasTexture(canvas);
}

async function buildStation(THREE, GLTFLoader, { tint = 0x6d7cff, size = 2.6 } = {}) {
  const group = new THREE.Group();
  const tinted = [];
  const rings = [];
  const orbiters = [];
  const ripples = [];
  let stars = null;
  let figure = null;
  let figureBaseY = 0;

  const makeGlow = (scale) => {
    const material = new THREE.SpriteMaterial({ map: glowTexture(THREE), color: tint, transparent: true, depthWrite: false, blending: THREE.AdditiveBlending });
    tinted.push(material);
    const sprite = new THREE.Sprite(material);
    sprite.scale.setScalar(scale);
    return sprite;
  };

  const orbitRing = (radius, y) => {
    const points = [];
    for (let i = 0; i <= 128; i += 1) {
      const a = (i / 128) * Math.PI * 2;
      points.push(new THREE.Vector3(Math.cos(a) * radius, y, Math.sin(a) * radius));
    }
    const material = new THREE.LineBasicMaterial({ color: tint, transparent: true, opacity: 0.16, depthWrite: false });
    rings.push(material);
    return new THREE.LineLoop(new THREE.BufferGeometry().setFromPoints(points), material);
  };

  const finish = (source) => ({
    group,
    source,
    update(delta, time, speed, pulse = 0.035) {
      for (const o of orbiters) o.pivot.rotation.y += delta * speed * o.speed;
      for (const r of ripples) {
        const wave = 1 + Math.sin(time * (0.8 + speed * 1.6) + r.offset * Math.PI) * pulse;
        r.object.scale.set(r.base.x * (r.vertical === 0 ? 1 : wave), r.base.y * (r.vertical === 1 ? 1 : wave), r.base.z * (r.vertical === 2 ? 1 : wave));
      }
      if (stars) stars.rotation.y += delta * speed * 0.06;
      if (figure) figure.position.y = figureBaseY + Math.sin(time * 1.2) * 0.012;
    },
    setTint(colour, ringOpacity) {
      for (const m of tinted) m.color.copy(colour);
      for (const m of rings) {
        m.color.copy(colour);
        if (ringOpacity !== undefined) m.opacity = ringOpacity;
      }
    },
    setRingColours(colours) {
      rings.forEach((m, i) => m.color.setHex(colours[i % colours.length]));
    },
  });

  try {
    const gltf = await new GLTFLoader().loadAsync(MODEL_URL);
    const model = gltf.scene;
    model.updateMatrixWorld(true);
    const parts = [];
    model.traverse((o) => {
      if (o === model || !o.name) return;
      if (SKIP.test(o.name) || BODY.test(o.name) || WAVES.test(o.name) || STARS.test(o.name) || PLANET.test(o.name)) parts.push(o);
    });
    for (const p of parts) if (SKIP.test(p.name)) p.visible = false;

    model.traverse((o) => {
      if (!o.isMesh) return;
      const list = Array.isArray(o.material) ? o.material : [o.material];
      const replaced = list.map((m) => {
        const clone = m.clone();
        if (m.color && m.color.getHex() === 0x000000) {
          clone.color = new THREE.Color(tint);
          tinted.push(clone);
        }
        return clone;
      });
      o.material = Array.isArray(o.material) ? replaced : replaced[0];
    });

    const focus = new THREE.Box3();
    for (const p of parts) if (BODY.test(p.name) || PLANET.test(p.name)) focus.expandByObject(p);
    if (focus.isEmpty()) focus.setFromObject(model);
    const extent = focus.getSize(new THREE.Vector3());
    const middle = focus.getCenter(new THREE.Vector3());
    const scale = size / Math.max(extent.x, extent.y, extent.z || 1);
    model.scale.setScalar(scale);
    model.position.sub(middle.clone().multiplyScalar(scale));
    group.add(model);
    model.updateMatrixWorld(true);

    const bodyPart = parts.find((p) => BODY.test(p.name));
    const axis = new THREE.Vector3();
    if (bodyPart) new THREE.Box3().setFromObject(bodyPart).getCenter(axis);
    figure = bodyPart || null;
    if (figure) figureBaseY = figure.position.y;

    for (const part of parts) {
      if (PLANET.test(part.name)) {
        const pos = new THREE.Box3().setFromObject(part).getCenter(new THREE.Vector3());
        const radius = Math.hypot(pos.x - axis.x, pos.z - axis.z) || 0.3;
        const pivot = new THREE.Group();
        pivot.position.set(axis.x, 0, axis.z);
        group.add(pivot);
        pivot.attach(part);
        orbiters.push({ pivot, speed: 0.9 / Math.sqrt(Math.max(0.2, radius)) });
      } else if (WAVES.test(part.name)) {
        const q = part.getWorldQuaternion(new THREE.Quaternion());
        const up = new THREE.Vector3(0, 1, 0);
        const align = [new THREE.Vector3(1, 0, 0), new THREE.Vector3(0, 1, 0), new THREE.Vector3(0, 0, 1)].map((v) => Math.abs(v.applyQuaternion(q).dot(up)));
        ripples.push({ object: part, base: part.scale.clone(), offset: ripples.length * 0.33, vertical: align.indexOf(Math.max(...align)) });
      } else if (STARS.test(part.name)) {
        const pivot = new THREE.Group();
        pivot.position.set(axis.x, 0, axis.z);
        group.add(pivot);
        pivot.attach(part);
        stars = pivot;
      }
    }

    const seen = new Set();
    for (const { pivot } of orbiters) {
      const child = pivot.children[0];
      if (!child) continue;
      const pos = new THREE.Box3().setFromObject(child).getCenter(new THREE.Vector3());
      const radius = Math.round(Math.hypot(pos.x - axis.x, pos.z - axis.z) * 20) / 20;
      if (seen.has(radius) || radius < 0.1) continue;
      seen.add(radius);
      const ring = orbitRing(radius, pos.y);
      ring.position.set(axis.x, 0, axis.z);
      group.add(ring);
    }
    const glow = makeGlow(size * 1.3);
    glow.position.set(axis.x, axis.y + 0.2, axis.z - 0.4);
    group.add(glow);
    return finish("model");
  } catch (error) {
    console.warn("station model unavailable, using the stand-in", error);
  }

  // Stand-in: a glowing core with moons on tilted orbits.
  const core = new THREE.MeshBasicMaterial({ color: tint });
  tinted.push(core);
  group.add(new THREE.Mesh(new THREE.SphereGeometry(size * 0.16, 48, 32), core));
  group.add(makeGlow(size * 0.9));
  const moon = new THREE.MeshBasicMaterial({ color: 0xdfe4ec });
  [0.36, 0.52, 0.68].forEach((fraction, i) => {
    const radius = size * fraction;
    const pivot = new THREE.Group();
    pivot.rotation.x = 0.35 - i * 0.28;
    const body = new THREE.Mesh(new THREE.SphereGeometry(size * (0.027 + i * 0.01), 24, 16), moon);
    body.position.set(radius, 0, 0);
    pivot.add(body);
    pivot.add(orbitRing(radius, 0));
    group.add(pivot);
    orbiters.push({ pivot, speed: 1 / Math.sqrt(radius) });
  });
  return finish("fallback");
}

const STATE_COLOUR = { idle: 0x6d7cff, training: 0x6d7cff, contributing: 0x6d7cff, hosting: 0x22d3ee, failed: 0xf76b6b };

let singleton = null;

/**
 * Attach the scene to `host` (the `.hero` element; its `.ld-canvas` child gets
 * the canvas). Returns a controller with setMood() and detach().
 */
export function attachScene(host) {
  const mount = host.querySelector(".ld-canvas");
  if (!webglAvailable()) {
    host.classList.add("scene-fallback");
    return { setMood() {}, detach() {}, source: "none" };
  }
  if (!singleton) singleton = createScene();
  return singleton.attach(host, mount);
}

function createScene() {
  const shared = { host: null, mount: null, ready: null, mood: { state: "idle", activity: 0 } };
  let three = null;

  shared.ready = (async () => {
    const THREE = await import("../vendor/three/three.module.min.js");
    const { GLTFLoader } = await import("../vendor/three/GLTFLoader.js");
    const scene = new THREE.Scene();
    scene.fog = new THREE.FogExp2(0x05060a, 0.035);
    const camera = new THREE.PerspectiveCamera(32, 16 / 9, 0.1, 400);
    camera.position.set(0, 0.4, 7);
    const renderer = new THREE.WebGLRenderer({ antialias: true, alpha: true, powerPreference: "low-power" });
    renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
    renderer.outputColorSpace = THREE.SRGBColorSpace;

    // Deep field: three shells of soft round stars drifting at different rates.
    const starCanvas = document.createElement("canvas");
    starCanvas.width = starCanvas.height = 64;
    const sctx = starCanvas.getContext("2d");
    const glow = sctx.createRadialGradient(32, 32, 0, 32, 32, 32);
    glow.addColorStop(0, "rgba(255,255,255,1)");
    glow.addColorStop(0.25, "rgba(255,255,255,0.85)");
    glow.addColorStop(0.6, "rgba(255,255,255,0.18)");
    glow.addColorStop(1, "rgba(255,255,255,0)");
    sctx.fillStyle = glow;
    sctx.fillRect(0, 0, 64, 64);
    const starSprite = new THREE.CanvasTexture(starCanvas);
    starSprite.colorSpace = THREE.SRGBColorSpace;
    const starLayers = [];
    for (const layer of [{ count: 1400, radius: 60, size: 0.05 }, { count: 600, radius: 30, size: 0.07 }, { count: 140, radius: 16, size: 0.11 }]) {
      const positions = new Float32Array(layer.count * 3);
      for (let i = 0; i < layer.count; i += 1) {
        const u = Math.random() * 2 - 1;
        const phi = Math.random() * Math.PI * 2;
        const r = layer.radius * (0.6 + Math.random() * 0.4);
        const s = Math.sqrt(1 - u * u);
        positions.set([r * s * Math.cos(phi), r * u * 0.6, r * s * Math.sin(phi) - layer.radius * 0.3], i * 3);
      }
      const geometry = new THREE.BufferGeometry();
      geometry.setAttribute("position", new THREE.BufferAttribute(positions, 3));
      const points = new THREE.Points(geometry, new THREE.PointsMaterial({ color: 0xcfd6ff, map: starSprite, size: layer.size * 1.8, sizeAttenuation: true, transparent: true, opacity: 0.75, depthWrite: false }));
      starLayers.push(points);
      scene.add(points);
    }

    const station = await buildStation(THREE, GLTFLoader, { tint: 0x6d7cff, size: 2.8 });
    station.setRingColours(VENDOR_RINGS);
    const pivot = new THREE.Group();
    const spin = new THREE.Group();
    spin.rotation.x = 0.08;
    spin.add(station.group);
    pivot.add(spin);
    scene.add(pivot);

    three = { THREE, scene, camera, renderer, station, pivot, spin, starLayers };
    return three;
  })();

  const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
  const pointer = { x: 0, y: 0 };
  let onScreen = true;
  let raf = 0;
  let last = performance.now();
  let elapsed = 0;
  const colour = { current: null, target: null };

  window.addEventListener(
    "pointermove",
    (e) => {
      pointer.x = e.clientX / window.innerWidth - 0.5;
      pointer.y = e.clientY / window.innerHeight - 0.5;
    },
    { passive: true }
  );

  // Light theme hides the scene (app.css), so it does not render either.
  const shouldRun = () => Boolean(shared.host && shared.host.isConnected && onScreen && !document.hidden && three && document.documentElement.dataset.theme !== "light");

  function frame(t) {
    raf = 0;
    if (!shouldRun()) return; // paused: nothing scheduled until something wakes it
    raf = requestAnimationFrame(frame);
    const delta = Math.min((t - last) / 1000, 0.1);
    last = t;
    elapsed += delta;
    const { camera, renderer, scene, station, pivot, spin, starLayers, THREE } = three;
    const still = reduceMotion.matches;
    const mood = shared.mood;
    const busy = mood.state === "training" || mood.state === "contributing";
    const speed = still ? 0 : busy ? 0.45 + mood.activity * 1.2 : 0.4;

    // Place the model in the right half of the hero, like v5's hero pose (x 0.6).
    const distance = 7;
    const halfH = Math.tan((camera.fov * Math.PI) / 360) * distance;
    const halfW = halfH * camera.aspect;
    pivot.position.set(0.52 * halfW * 0.92, -0.02 * halfH, 0);
    camera.position.x += (pointer.x * 0.35 - camera.position.x) * Math.min(1, delta * 2);
    camera.position.y += (0.4 - pointer.y * 0.25 - camera.position.y) * Math.min(1, delta * 2);
    camera.position.z = distance;
    camera.lookAt(pivot.position.x * 0.15, pivot.position.y * 0.15, 0);

    if (!still) {
      spin.rotation.y += delta * 0.16;
      starLayers.forEach((layer, i) => (layer.rotation.y += delta * (0.004 + i * 0.003)));
    }
    station.update(delta, elapsed, speed, still ? 0 : 0.04);
    if (!colour.current) {
      colour.current = new THREE.Color(STATE_COLOUR.idle);
      colour.target = new THREE.Color();
    }
    colour.target.setHex(STATE_COLOUR[mood.state] ?? STATE_COLOUR.idle);
    colour.current.lerp(colour.target, Math.min(1, delta * 3));
    station.setTint(colour.current, busy ? 0.24 : 0.16);
    if (mood.state !== "hosting" && mood.state !== "failed") station.setRingColours(VENDOR_RINGS);
    renderer.render(scene, camera);
  }

  function wake() {
    if (!raf && shouldRun()) {
      last = performance.now();
      raf = requestAnimationFrame(frame);
    }
  }

  document.addEventListener("visibilitychange", wake);
  window.addEventListener("gradapp:theme", wake);
  reduceMotion.addEventListener?.("change", wake);

  const resizeObserver = new ResizeObserver(() => resize());
  const intersection = new IntersectionObserver(([entry]) => {
    onScreen = Boolean(entry?.isIntersecting);
    wake();
  });

  function resize() {
    if (!three || !shared.mount) return;
    const w = shared.mount.clientWidth;
    const hgt = shared.mount.clientHeight;
    if (!w || !hgt) return;
    three.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 1.5));
    three.renderer.setSize(w, hgt, false);
    three.camera.aspect = w / hgt;
    three.camera.updateProjectionMatrix();
  }

  return {
    attach(host, mount) {
      shared.host = host;
      shared.mount = mount;
      const controller = {
        source: "loading",
        setMood(state, activity = 0) {
          shared.mood = { state, activity };
        },
        detach() {
          if (shared.host === host) {
            resizeObserver.disconnect();
            intersection.disconnect();
            shared.host = null;
            shared.mount = null;
          }
        },
      };
      shared.ready
        .then((t) => {
          if (shared.host !== host) return;
          mount.append(t.renderer.domElement);
          resizeObserver.observe(mount);
          intersection.observe(host);
          resize();
          controller.source = t.station.source;
          host.classList.add("scene-ready");
          host.dispatchEvent(new CustomEvent("scene-source", { detail: t.station.source }));
          wake();
        })
        .catch((error) => {
          console.warn("3D scene unavailable", error);
          host.classList.add("scene-fallback");
        });
      return controller;
    },
  };
}
