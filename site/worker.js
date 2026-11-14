/**
 * Static asset server for the Vanta site.
 *
 * Every request is served from the uploaded `public/` bundle; there are no API
 * routes, no bindings and no server state.
 */
export default {
  async fetch(request, env) {
    return env.ASSETS.fetch(request);
  },
};
