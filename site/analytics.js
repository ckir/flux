/**
 * Copyright (c) 2026 Costas Kirgoussios
 *
 * This software is licensed under the PolyForm Noncommercial License 1.0.0.
 * You may obtain a copy of the License at:
 * https://polyformproject.org/licenses/noncommercial/1.0.0/
 *
 * Flux Provider-Independent Analytics & Telemetry Layer
 *
 * Requirements:
 * - Zero third-party runtime dependencies.
 * - No cookies, no local storage fingerprinting, no PII.
 * - Captures UTM campaign parameters for attribution.
 * - Safe fallback: no-ops gracefully when unconfigured.
 */

(function () {
  'use strict';

  function getUtmParams() {
    var params = {};
    if (!window.location.search) { return params; }
    var search = window.location.search.substring(1);
    var pairs = search.split('&');
    for (var i = 0; i < pairs.length; i++) {
      var pair = pairs[i].split('=');
      var key = decodeURIComponent(pair[0]);
      if (key.indexOf('utm_') === 0 && pair.length > 1) {
        params[key] = decodeURIComponent(pair[1] || '');
      }
    }
    return params;
  }

  var utm = getUtmParams();
  var eventQueue = [];

  var FluxAnalytics = {
    track: function (eventName, properties) {
      properties = properties || {};
      delete properties.email;
      delete properties.password;
      delete properties.token;

      for (var k in utm) {
        if (utm.hasOwnProperty(k) && !properties[k]) {
          properties[k] = utm[k];
        }
      }

      var payload = {
        name: eventName,
        properties: properties,
        path: window.location.pathname,
        timestamp: new Date().toISOString()
      };

      eventQueue.push(payload);

      if (typeof window.__FLUX_ANALYTICS_DISPATCHER__ === 'function') {
        try {
          window.__FLUX_ANALYTICS_DISPATCHER__(payload);
        } catch (e) {
          // Fail closed and silent
        }
      }
    },

    getQueue: function () {
      return eventQueue.slice();
    }
  };

  window.FluxAnalytics = FluxAnalytics;

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', onReady);
  } else {
    onReady();
  }

  function onReady() {
    FluxAnalytics.track('page_view', {
      title: document.title,
      referrer: document.referrer || ''
    });

    document.addEventListener('click', function (e) {
      var target = e.target.closest('[data-analytics-event]');
      if (!target) { return; }

      var eventName = target.getAttribute('data-analytics-event');
      var propString = target.getAttribute('data-analytics-props');
      var props = {};

      if (propString) {
        try {
          props = JSON.parse(propString);
        } catch (err) {
          // ignore
        }
      }

      if (target.href && !props.target_url) {
        props.target_url = target.href;
      }

      FluxAnalytics.track(eventName, props);
    }, false);
  }
})();
