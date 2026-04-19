/// Policy for how watcher-triggered refreshes choose probe scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeWatchRefreshMode {
    /// Refresh only the interfaces explicitly touched by watch events. If no
    /// scoped probe set can be built, skip the refresh.
    StrictScoped,
    /// Prefer a scoped refresh, but fall back to the caller's full probe set
    /// when the watcher did not provide enough interface context.
    FallbackToFull,
    /// Always use the full caller-provided probe set for watcher-driven
    /// refreshes.
    Full,
}

/// Runtime-wide behavior knobs for binding, state retention, and watch-driven
/// refresh handling.
///
/// These settings are synchronous runtime policy. Backend-local transport
/// defaults, such as Linux UART or USB timeouts, stay in backend-specific
/// configuration rather than this struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeConfig {
    /// Automatically bind a device on first request if it is discovered but not
    /// currently bound.
    pub auto_bind_on_request: bool,
    /// Attempt to rebind previously desired devices after refresh changes.
    pub auto_rebind_on_refresh: bool,
    /// Cache a state snapshot immediately after a successful bind.
    pub cache_state_on_bind: bool,
    /// Cache a state snapshot after successful requests when the bound device
    /// can report one.
    pub cache_state_on_request: bool,
    /// Maximum number of retained runtime events kept in memory.
    pub max_retained_events: usize,
    /// Optional approximate byte budget for retained runtime events.
    pub max_retained_event_bytes: Option<usize>,
    /// Strategy used when hotplug/watch events trigger a refresh.
    pub watch_refresh_mode: RuntimeWatchRefreshMode,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeConfig {
    pub const DEFAULT_AUTO_BIND_ON_REQUEST: bool = true;
    pub const DEFAULT_AUTO_REBIND_ON_REFRESH: bool = true;
    pub const DEFAULT_CACHE_STATE_ON_BIND: bool = true;
    pub const DEFAULT_CACHE_STATE_ON_REQUEST: bool = true;
    pub const DEFAULT_MAX_RETAINED_EVENTS: usize = 1024;
    pub const DEFAULT_MAX_RETAINED_EVENT_BYTES: Option<usize> = None;
    pub const DEFAULT_WATCH_REFRESH_MODE: RuntimeWatchRefreshMode =
        RuntimeWatchRefreshMode::StrictScoped;

    pub const fn new() -> Self {
        Self {
            auto_bind_on_request: Self::DEFAULT_AUTO_BIND_ON_REQUEST,
            auto_rebind_on_refresh: Self::DEFAULT_AUTO_REBIND_ON_REFRESH,
            cache_state_on_bind: Self::DEFAULT_CACHE_STATE_ON_BIND,
            cache_state_on_request: Self::DEFAULT_CACHE_STATE_ON_REQUEST,
            max_retained_events: Self::DEFAULT_MAX_RETAINED_EVENTS,
            max_retained_event_bytes: Self::DEFAULT_MAX_RETAINED_EVENT_BYTES,
            watch_refresh_mode: Self::DEFAULT_WATCH_REFRESH_MODE,
        }
    }

    pub const fn with_auto_bind_on_request(mut self, auto_bind_on_request: bool) -> Self {
        self.auto_bind_on_request = auto_bind_on_request;
        self
    }

    pub const fn with_auto_rebind_on_refresh(mut self, auto_rebind_on_refresh: bool) -> Self {
        self.auto_rebind_on_refresh = auto_rebind_on_refresh;
        self
    }

    pub const fn with_cache_state_on_bind(mut self, cache_state_on_bind: bool) -> Self {
        self.cache_state_on_bind = cache_state_on_bind;
        self
    }

    pub const fn with_cache_state_on_request(mut self, cache_state_on_request: bool) -> Self {
        self.cache_state_on_request = cache_state_on_request;
        self
    }

    pub const fn with_max_retained_events(mut self, max_retained_events: usize) -> Self {
        self.max_retained_events = max_retained_events;
        self
    }

    pub const fn with_max_retained_event_bytes(
        mut self,
        max_retained_event_bytes: Option<usize>,
    ) -> Self {
        self.max_retained_event_bytes = max_retained_event_bytes;
        self
    }

    pub const fn with_watch_refresh_mode(
        mut self,
        watch_refresh_mode: RuntimeWatchRefreshMode,
    ) -> Self {
        self.watch_refresh_mode = watch_refresh_mode;
        self
    }
}
