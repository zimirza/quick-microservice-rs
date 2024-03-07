use std::path::Path;
use std::sync::Arc;

use cucumber::event::Cucumber;
use cucumber::gherkin::Feature;
use cucumber::{writer, Event, World as _, WriterExt};
use futures::FutureExt;
use qm::customer::schema::customer::CustomerDB;
use tokio::sync::RwLock;

use crate::world::{init_context, Ctx, World};

mod definitions;
mod steps;
mod utils;
mod world;

async fn before(_f: &Feature, w: &mut World, ctx: Ctx) {
    if w.ctx.is_none() {
        w.init(ctx).await.expect("unable to set world");
    }
}

#[derive(Default)]
pub struct Stats {
    pub failed: usize,
    pub skipped: usize,
}

pub struct CustomWriter {
    pub stats: Arc<RwLock<Stats>>,
}

impl CustomWriter {
    pub fn new<W>(stats: Arc<RwLock<Stats>>) -> writer::Normalize<W, Self> {
        Self { stats }.normalized()
    }
}

#[async_trait::async_trait(?Send)]
impl<W: 'static> cucumber::Writer<W> for CustomWriter {
    type Cli = cucumber::cli::Empty;

    async fn handle_event(
        &mut self,
        ev: cucumber::parser::Result<Event<Cucumber<W>>>,
        cli: &Self::Cli,
    ) {
        use cucumber::{event, Event};
        match ev {
            Ok(Event { value, .. }) => match value {
                Cucumber::Feature(_feature, ev) => match ev {
                    event::Feature::Scenario(_scenario, ev) => match ev.event {
                        event::Scenario::Step(_step, ev) => match ev {
                            event::Step::Skipped => {
                                let mut s = self.stats.write().await;
                                s.skipped += 1;
                            }
                            event::Step::Failed(_, _, _, _) => {
                                let mut s = self.stats.write().await;
                                s.failed += 1;
                            }
                            _ => {}
                        },
                        _ => {}
                    },
                    _ => {}
                },
                _ => {}
            },
            Err(e) => println!("Error: {e}"),
            _ => {}
        }
    }
}

async fn run_with_tag(
    ctx: Ctx,
    path: &Path,
    input: &'static str,
    tags: &'static [&'static str],
    stats: Arc<RwLock<Stats>>,
) -> anyhow::Result<()> {
    let filename = format!(
        "junit-{}_{}.xml",
        input.replace('/', "_"),
        tags.join("-or-")
    );
    let file = std::fs::File::create(path.join(&filename))?;
    let result = World::cucumber()
        .fail_on_skipped()
        .with_writer(
            writer::libtest::Libtest::or_basic()
                .tee(writer::JUnit::new(file, 0))
                .tee(CustomWriter::new(stats)),
        )
        .before(move |f, _, _, w| {
            let ctx = ctx.clone();
            async move {
                before(f, w, ctx).await;
            }
            .boxed()
        })
        .filter_run(&format!("tests/features/{input}"), |f, _, _sc| {
            f.tags.iter().any(|t| tags.contains(&t.as_str()))
        })
        .await;

    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let stats: Arc<RwLock<Stats>> = Default::default();
    dotenv::from_filename(std::env::var("TEST_ENV").as_deref().unwrap_or(".env.test"))
        .expect("unable to read dotfile");
    let _ = env_logger::init();
    let ctx = init_context().await?;
    let path = Path::new("../../target/junit");
    if !path.exists() {
        std::fs::create_dir_all(path)?;
    } else {
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            std::fs::remove_file(entry.path())?;
        }
    }
    run_with_tag(
        ctx.clone(),
        path,
        "administration",
        &["customers"],
        stats.clone(),
    )
    .await?;
    let cleanup_after_env = std::env::var("CLEANUP_INFRA_AFTER");
    let cleanup = cleanup_after_env.as_deref() == Ok("true");
    if cleanup {
        let realm = ctx.store.keycloak().config().realm();
        ctx.store.customer_db().cleanup().await?;
        ctx.store.keycloak().remove_realm(realm).await?;
    }
    let failed = stats.read().await.failed;
    let skipped = stats.read().await.skipped;
    if failed > 0 || skipped > 0 {
        log::error!("Failed: {}, Skipped: {}", failed, skipped);
        std::process::exit(1);
    }
    Ok(())
}
