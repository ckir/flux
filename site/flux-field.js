// Decorative flux-field animation for the hero canvas. No dependencies.
(function () {
  "use strict";
  var canvas = document.getElementById("flux-field");
  var ctx = canvas && canvas.getContext && canvas.getContext("2d");
  if (!ctx) { return; }
  var motion = window.matchMedia("(prefers-reduced-motion: reduce)");
  var colours = ["0,240,255", "112,0,255", "59,130,246"];
  var w = 0, h = 0, t = 0, raf = 0, dots = [];

  function resize() {
    var r = canvas.getBoundingClientRect();
    w = canvas.width = Math.max(1, Math.floor(r.width));
    h = canvas.height = Math.max(1, Math.floor(r.height));
    var n = Math.min(90, Math.floor(w / 12));
    dots = [];
    for (var i = 0; i < n; i++) {
      dots.push({ x: Math.random() * w, y: Math.random() * h, s: 0.3 + Math.random() * 0.9, c: colours[i % 3] });
    }
  }

  function frame() {
    ctx.clearRect(0, 0, w, h);
    for (var i = 0; i < dots.length; i++) {
      var d = dots[i];
      var a = Math.sin(d.x / 140 + t) + Math.cos(d.y / 110 - t);
      d.x += Math.cos(a) * d.s;
      d.y += Math.sin(a) * d.s;
      if (d.x < 0) { d.x = w; } else if (d.x > w) { d.x = 0; }
      if (d.y < 0) { d.y = h; } else if (d.y > h) { d.y = 0; }
      ctx.fillStyle = "rgba(" + d.c + ",0.55)";
      ctx.fillRect(d.x, d.y, 2, 2);
    }
    t += 0.004;
  }

  function loop() {
    frame();
    raf = window.requestAnimationFrame(loop);
  }

  function stop() {
    if (raf) { window.cancelAnimationFrame(raf); raf = 0; }
  }

  function sync() {
    stop();
    if (motion.matches) {
      for (var i = 0; i < 60; i++) { frame(); }
      return;
    }
    if (!document.hidden) { loop(); }
  }

  resize();
  sync();
  window.addEventListener("resize", function () { resize(); sync(); });
  document.addEventListener("visibilitychange", sync);
  if (motion.addEventListener) { motion.addEventListener("change", sync); }
})();
