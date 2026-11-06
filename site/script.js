/**
 * Copyright (c) 2026 Costas Kirgoussios
 *
 * This software is licensed under the PolyForm Noncommercial License 1.0.0.
 * You may obtain a copy of the License at:
 * https://polyformproject.org/licenses/noncommercial/1.0.0/
 */

// Copy buttons only. The page works without JS: every command is plain text.
var statusTimers = [];
document.querySelectorAll('#install button[data-copy], .quickstart button[data-copy], .commands button[data-copy]').forEach(function (btn) {
  var label = btn.textContent;
  var status = document.getElementById('copy-status');
  var what = (btn.getAttribute('aria-label') || '').replace(/^Copy command: /, '');
  var cmd = btn.getAttribute('data-copy');

  var say = function (text) {
    if (!status) { return; }
    statusTimers.forEach(clearTimeout);
    status.textContent = '';
    statusTimers = [
      setTimeout(function () { status.textContent = text; }, 50),
      setTimeout(function () { status.textContent = ''; }, 1550)
    ];
  };

  btn.addEventListener('click', function () {
    var done = function () {
      btn.textContent = 'Copied';
      say('Copied: ' + what);
      if (window.FluxAnalytics) {
        window.FluxAnalytics.track('install_command_copied', { command: cmd });
      }
      setTimeout(function () { btn.textContent = label; }, 1500);
    };

    if (navigator.clipboard && navigator.clipboard.writeText) {
      navigator.clipboard.writeText(cmd).then(done, select);
    } else {
      select();
    }

    function select() {
      var code = btn.parentElement.querySelector('code');
      if (!code) { return; }
      say('Selected for copying: press Ctrl+C. ' + what);
      var range = document.createRange();
      range.selectNodeContents(code);
      var sel = window.getSelection();
      sel.removeAllRanges();
      sel.addRange(range);
    }
  });
});

// Download / Evaluation form handler (for /download/)
(function () {
  var form = document.getElementById('eval-intake-form');
  if (!form) { return; }

  var statusDiv = document.getElementById('form-status');
  var submitBtn = document.getElementById('form-submit-btn');
  var hasStarted = false;

  form.addEventListener('focusin', function () {
    if (!hasStarted) {
      hasStarted = true;
      if (window.FluxAnalytics) {
        window.FluxAnalytics.track('eval_form_started', { form_id: 'eval-intake-form' });
      }
    }
  });

  var commCheckbox = document.getElementById('commercial-interest');
  if (commCheckbox) {
    commCheckbox.addEventListener('change', function () {
      if (commCheckbox.checked && window.FluxAnalytics) {
        window.FluxAnalytics.track('commercial_interest_selected', {});
      }
    });
  }

  form.addEventListener('submit', function (e) {
    e.preventDefault();

    var email = (document.getElementById('email') || {}).value || '';
    var role = (document.getElementById('role') || {}).value || '';
    var os = (document.getElementById('os') || {}).value || '';
    var privacy = (document.getElementById('privacy') || {}).checked;
    var commercial = commCheckbox ? commCheckbox.checked : false;

    if (statusDiv) {
      statusDiv.className = 'status-msg';
      statusDiv.textContent = '';
    }

    var emailRegex = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;
    if (!emailRegex.test(email.trim())) {
      showError('Please enter a valid developer email address.');
      return;
    }
    if (!role) {
      showError('Please select your developer or team profile.');
      return;
    }
    if (!privacy) {
      showError('Please acknowledge the noncommercial license agreement to continue.');
      return;
    }

    var payload = {
      email: email.trim(),
      role: role,
      os: os,
      commercialInterest: commercial,
      submittedAt: new Date().toISOString()
    };

    if (window.FluxAnalytics) {
      window.FluxAnalytics.track('eval_form_submitted', {
        role_type: role,
        has_commercial_interest: commercial,
        target_os: os
      });
    }

    var endpoint = form.getAttribute('data-endpoint') || window.__FLUX_EVAL_ENDPOINT__;

    if (endpoint) {
      submitBtn.disabled = true;
      submitBtn.textContent = 'Submitting...';

      fetch(endpoint, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(payload)
      })
      .then(function (res) {
        if (!res.ok) { throw new Error('Server returned ' + res.status); }
        return res.json();
      })
      .then(function () {
        showSuccess('Evaluation registration confirmed! Check your inbox for updates.');
        form.reset();
      })
      .catch(function (err) {
        showError('Unable to connect to submission service: ' + err.message + '. Please use GitHub Discussions.');
      })
      .finally(function () {
        submitBtn.disabled = false;
        submitBtn.textContent = 'Submit Registration';
      });
    } else {
      showSuccess('Interest recorded (Preview Mode)! Join us on GitHub Discussions for evaluation feedback.');
      form.reset();
    }
  });

  function showError(msg) {
    if (!statusDiv) { return; }
    statusDiv.className = 'status-msg error';
    statusDiv.setAttribute('role', 'alert');
    statusDiv.textContent = msg;
  }

  function showSuccess(msg) {
    if (!statusDiv) { return; }
    statusDiv.className = 'status-msg success';
    statusDiv.setAttribute('role', 'status');
    statusDiv.textContent = msg;
  }
})();
