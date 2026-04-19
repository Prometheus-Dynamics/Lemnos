#[macro_export]
macro_rules! impl_bound_device_core {
    ($session_field:ident, fallible $snapshot_method:ident) => {
        fn device(&self) -> &lemnos_core::DeviceDescriptor {
            self.$session_field.device()
        }

        fn driver_id(&self) -> &str {
            self.driver_id.as_str()
        }

        fn close(&mut self) -> $crate::DriverResult<()> {
            $crate::close_session(self.driver_id.as_str(), self.$session_field.as_mut())
        }

        fn state(&mut self) -> $crate::DriverResult<Option<lemnos_core::DeviceStateSnapshot>> {
            Ok(Some(self.$snapshot_method()?))
        }
    };
    ($session_field:ident, infallible $snapshot_method:ident) => {
        fn device(&self) -> &lemnos_core::DeviceDescriptor {
            self.$session_field.device()
        }

        fn driver_id(&self) -> &str {
            self.driver_id.as_str()
        }

        fn close(&mut self) -> $crate::DriverResult<()> {
            $crate::close_session(self.driver_id.as_str(), self.$session_field.as_mut())
        }

        fn state(&mut self) -> $crate::DriverResult<Option<lemnos_core::DeviceStateSnapshot>> {
            Ok(Some(self.$snapshot_method()))
        }
    };
}

#[macro_export]
macro_rules! impl_session_io {
    ($method:ident, $session_field:ident, $io_ty:ident) => {
        fn $method(&mut self) -> $crate::$io_ty<'_> {
            let device_id = self.$session_field.device().id.clone();
            $crate::$io_ty::with_device_id(
                self.$session_field.as_mut(),
                self.driver_id.as_str(),
                device_id,
            )
        }
    };
}

#[macro_export]
macro_rules! execute_standard_request {
    ($self:ident, $request:ident, $variant:ident($standard_request:ident) => $response_variant:ident, $body:expr) => {{
        $crate::validate_request_for_device(
            $self.driver_id.as_str(),
            $self.session.device(),
            $request,
        )?;

        match $request {
            lemnos_core::InteractionRequest::Standard(lemnos_core::StandardRequest::$variant(
                $standard_request,
            )) => Ok(lemnos_core::InteractionResponse::Standard(
                lemnos_core::StandardResponse::$response_variant($body),
            )),
            _ => Err($crate::unsupported_action_error(
                $self.driver_id.as_str(),
                $self.session.device(),
                $request,
            )),
        }
    }};
}

#[macro_export]
macro_rules! define_session_driver {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        id: $id:expr;
        interface: $interface:expr;
        manifest: $manifest:path;
        kind: $kind:expr;
        expected: $expected:expr;
        open: |$open_context:ident, $open_driver_id:ident, $open_device:ident| $open_expr:expr;
        build: |$build_driver_id:ident, $build_session:ident| $build_expr:expr $(;)?
    ) => {
        $(#[$meta])*
        $vis struct $name;

        impl $crate::Driver for $name {
            fn id(&self) -> &str {
                $id
            }

            fn interface(&self) -> lemnos_core::InterfaceKind {
                $interface
            }

            fn manifest_ref(
                &self,
            ) -> ::std::borrow::Cow<'static, lemnos_driver_manifest::DriverManifest> {
                ::std::borrow::Cow::Borrowed($manifest())
            }

            fn bind(
                &self,
                device: &lemnos_core::DeviceDescriptor,
                context: &$crate::DriverBindContext<'_>,
            ) -> $crate::DriverResult<Box<dyn $crate::BoundDevice>> {
                $crate::bind_session_for_kind(
                    self.id(),
                    device,
                    $kind,
                    $expected,
                    || {
                        let $open_context = context;
                        let $open_driver_id = self.id();
                        let $open_device = device;
                        $open_expr
                    },
                    |$build_driver_id, $build_session| $build_expr,
                )
            }
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        id: $id:expr;
        interface: $interface:expr;
        manifest: $manifest:path;
        kinds: $kinds:expr;
        expected: $expected:expr;
        open: |$open_context:ident, $open_driver_id:ident, $open_device:ident| $open_expr:expr;
        build: |$build_driver_id:ident, $build_session:ident| $build_expr:expr $(;)?
    ) => {
        $(#[$meta])*
        $vis struct $name;

        impl $crate::Driver for $name {
            fn id(&self) -> &str {
                $id
            }

            fn interface(&self) -> lemnos_core::InterfaceKind {
                $interface
            }

            fn manifest_ref(
                &self,
            ) -> ::std::borrow::Cow<'static, lemnos_driver_manifest::DriverManifest> {
                ::std::borrow::Cow::Borrowed($manifest())
            }

            fn bind(
                &self,
                device: &lemnos_core::DeviceDescriptor,
                context: &$crate::DriverBindContext<'_>,
            ) -> $crate::DriverResult<Box<dyn $crate::BoundDevice>> {
                $crate::bind_session_for_kinds(
                    self.id(),
                    device,
                    $kinds,
                    $expected,
                    || {
                        let $open_context = context;
                        let $open_driver_id = self.id();
                        let $open_device = device;
                        $open_expr
                    },
                    |$build_driver_id, $build_session| $build_expr,
                )
            }
        }
    };
}

#[macro_export]
macro_rules! define_generic_driver_manifest {
    (
        id: $id:expr;
        summary: $summary:expr;
        interface: $interface:expr;
        kind: $kind:expr;
        interactions: $interactions:expr $(;)?
    ) => {
        pub fn manifest() -> &'static lemnos_driver_manifest::DriverManifest {
            static MANIFEST: ::std::sync::OnceLock<lemnos_driver_manifest::DriverManifest> =
                ::std::sync::OnceLock::new();

            $crate::cached_manifest(&MANIFEST, || {
                $crate::generic_driver_manifest_with_standard_interactions(
                    $id,
                    $summary,
                    $interface,
                    &[$kind],
                    $interactions,
                )
            })
        }
    };
    (
        id: $id:expr;
        summary: $summary:expr;
        interface: $interface:expr;
        kinds: $kinds:expr;
        interactions: $interactions:expr $(;)?
    ) => {
        pub fn manifest() -> &'static lemnos_driver_manifest::DriverManifest {
            static MANIFEST: ::std::sync::OnceLock<lemnos_driver_manifest::DriverManifest> =
                ::std::sync::OnceLock::new();

            $crate::cached_manifest(&MANIFEST, || {
                $crate::generic_driver_manifest_with_standard_interactions(
                    $id,
                    $summary,
                    $interface,
                    $kinds,
                    $interactions,
                )
            })
        }
    };
}

#[macro_export]
macro_rules! define_generic_session_driver {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        id: $id:expr;
        interface: $interface:expr;
        manifest: $manifest:path;
        kind: $kind:expr;
        expected: $expected:expr;
        open: $open_method:ident;
        access: $access:ident;
        bound: $bound:path;
        stats: $stats:path $(;)?
    ) => {
        $crate::define_session_driver! {
            $(#[$meta])*
            $vis struct $name;
            id: $id;
            interface: $interface;
            manifest: $manifest;
            kind: $kind;
            expected: $expected;
            open: |context, driver_id, device| context.$open_method(
                driver_id,
                device,
                $crate::SessionAccess::$access,
            );
            build: |driver_id, session| {
                $bound {
                    driver_id,
                    session,
                    stats: <$stats>::default(),
                }
            };
        }
    };
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident;
        id: $id:expr;
        interface: $interface:expr;
        manifest: $manifest:path;
        kinds: $kinds:expr;
        expected: $expected:expr;
        open: $open_method:ident;
        access: $access:ident;
        bound: $bound:path;
        stats: $stats:path $(;)?
    ) => {
        $crate::define_session_driver! {
            $(#[$meta])*
            $vis struct $name;
            id: $id;
            interface: $interface;
            manifest: $manifest;
            kinds: $kinds;
            expected: $expected;
            open: |context, driver_id, device| context.$open_method(
                driver_id,
                device,
                $crate::SessionAccess::$access,
            );
            build: |driver_id, session| {
                $bound {
                    driver_id,
                    session,
                    stats: <$stats>::default(),
                }
            };
        }
    };
}
