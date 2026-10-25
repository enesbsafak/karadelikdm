//! Karadelik Download Manager çekirdeği: indirme motoru ve arayüzden bağımsız mantık.

pub mod category;
pub mod engine;
pub mod queue;
pub mod quota;

/// Uygulama aynı HTTP istemcisini kullansın: ayrı bağımlılık gzip özelliğini açıp parçalı indirmeyi bozardı
pub use ureq;
