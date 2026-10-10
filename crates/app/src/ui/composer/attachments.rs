//! Draft attachments and background sends (`attachment-draft.ts`,
//! `background-upload.ts`).
//!
//! Files wait in a draft, queued, until Send. Send hands them to a
//! background job of the community's queue and clears the composer at
//! once; the job uploads them in order, then the composer's host publishes
//! the message. A job that fails or is cancelled hands its files and the
//! sent text back to the draft. Drafts and jobs outlive their composer, so
//! files survive navigation (not a restart), as in buzz-app's tab.

use crate::community::Community;
use buzz_relay_client::upload::{self, MAX_UPLOAD, Source, Stage, Upload, UploadError};
use buzz_relay_client::{Attachment, Media};
use futures::StreamExt;
use futures::channel::{mpsc, oneshot};
use gpui_kit::*;
use std::collections::HashMap;
use std::sync::Arc;

/// At most this many files per message.
pub const MAX_FILES: usize = 10;
/// Bytes all drafts and sends of a community may hold (`MAX_RETAINED_BYTES`).
const MAX_RETAINED: u64 = 2 * MAX_UPLOAD;

/// A recorded voice note: it plays from memory under `key` until it is sent.
#[derive(Clone, Debug)]
pub struct Voice {
    pub key: String,
    pub duration: f64,
    pub waveform: Vec<u8>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Queued,
    Preparing,
    Uploading,
    Ready,
    Error,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Queued => "Queued",
            Status::Preparing => "Preparing…",
            Status::Uploading => "Uploading…",
            Status::Ready => "Ready",
            Status::Error => "Upload failed",
        }
    }

    /// Preparing or transferring; queued files wait for Send.
    pub fn busy(self) -> bool {
        matches!(self, Status::Preparing | Status::Uploading)
    }
}

/// One file of a draft (`DraftAttachment`).
#[derive(Clone, Debug)]
pub struct DraftFile {
    pub id: String,
    pub name: String,
    /// The type the name or bytes suggest; the relay's wins once uploaded.
    pub mime: String,
    pub size: u64,
    pub source: Source,
    pub voice: Option<Voice>,
    pub status: Status,
    pub uploaded: Option<Attachment>,
    pub error: Option<SharedString>,
    /// Bytes handed to the connection, of the prepared total.
    pub transfer: Option<(u64, u64)>,
}

/// A file being added.
pub struct NewFile {
    pub name: String,
    pub mime: String,
    pub size: u64,
    pub source: Source,
    pub voice: Option<Voice>,
}

/// A failed send's text, handed back to whichever composer shows its
/// draft.
#[derive(Clone)]
pub struct Recovered {
    pub draft: String,
    /// The reason, for the composer's error line.
    pub error: Option<SharedString>,
}

/// The files of one draft (`AttachmentDraft`).
pub struct AttachmentDraft {
    media: Media,
    pub items: Vec<DraftFile>,
    /// Uploads running, file ID to upload ID.
    active: HashMap<String, String>,
    /// Composers showing this draft.
    viewers: usize,
    /// Text a failed send handed back, until a composer takes it.
    pub recovered: Option<Recovered>,
}

impl AttachmentDraft {
    fn new(media: Media) -> Self {
        Self {
            media,
            items: Vec::new(),
            active: HashMap::new(),
            viewers: 0,
            recovered: None,
        }
    }

    pub fn retained(&self) -> u64 {
        self.items.iter().map(|item| item.size).sum()
    }

    /// Preparing, uploading or failed files block Send (`blocked`).
    pub fn blocked(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.status.busy() || item.status == Status::Error)
    }

    fn replace(&mut self, id: &str, cx: &mut Context<Self>, change: impl FnOnce(&mut DraftFile)) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            change(item);
            cx.notify();
        }
    }

    /// Adds files after the checks of `add`; `retained` counts every other
    /// draft and send of the community.
    fn add(
        &mut self,
        files: Vec<NewFile>,
        retained: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.items.len() + files.len() > MAX_FILES {
            return Err(format!("Attach at most {MAX_FILES} files per message."));
        }
        if files
            .iter()
            .any(|file| file.size == 0 || file.size > MAX_UPLOAD)
        {
            return Err(UploadError::Size.to_string());
        }
        let adding: u64 = files.iter().map(|file| file.size).sum();
        if retained + self.retained() + adding > MAX_RETAINED {
            return Err(
                "Attachment drafts are full (1,000 MiB). Send or remove some files first.".into(),
            );
        }
        self.items.extend(files.into_iter().map(|file| DraftFile {
            id: uuid::Uuid::new_v4().to_string(),
            name: file.name,
            mime: file.mime,
            size: file.size,
            source: file.source,
            voice: file.voice,
            status: Status::Queued,
            uploaded: None,
            error: None,
            transfer: None,
        }));
        cx.notify();
        Ok(())
    }

    /// Adds a file already on the relay, such as the one a pasted snapshot
    /// link names: it sends as it is, without another upload.
    pub fn add_uploaded(
        &mut self,
        attachment: Attachment,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if self.items.len() >= MAX_FILES {
            return Err(format!("Attach at most {MAX_FILES} files per message."));
        }
        self.items.push(DraftFile {
            id: uuid::Uuid::new_v4().to_string(),
            name: attachment.name.clone(),
            mime: attachment.mime.clone(),
            size: attachment.size,
            source: Source::Bytes(Arc::from(Vec::new())),
            voice: None,
            status: Status::Ready,
            uploaded: Some(attachment),
            error: None,
            transfer: None,
        });
        cx.notify();
        Ok(())
    }

    /// Moves files, with their results and errors, into an empty draft.
    fn adopt(&mut self, files: Vec<DraftFile>, cx: &mut Context<Self>) -> bool {
        if !self.items.is_empty() || files.is_empty() {
            return false;
        }
        self.items = files;
        cx.notify();
        true
    }

    /// Removes a file, stopping its upload.
    pub fn remove(&mut self, id: &str, cx: &mut Context<Self>) {
        if let Some(upload) = self.active.remove(id) {
            upload::cancel(&upload);
        }
        self.items.retain(|item| item.id != id);
        cx.notify();
    }

    /// Queues a failed file again; the next send uploads it from scratch.
    pub fn retry(&mut self, id: &str, cx: &mut Context<Self>) {
        self.replace(id, cx, |item| {
            if item.status == Status::Error {
                item.status = Status::Queued;
                item.error = None;
                item.transfer = None;
            }
        });
    }

    /// Stops every running upload; those files wait for a retry
    /// ("Upload paused").
    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        for (_, upload) in self.active.drain() {
            upload::cancel(&upload);
        }
        for item in &mut self.items {
            if item.status.busy() {
                item.status = Status::Error;
                item.error = Some("Upload paused. Retry to continue.".into());
                item.transfer = None;
            }
        }
        cx.notify();
    }

    /// Drops every file, stopping their uploads.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        for (_, upload) in self.active.drain() {
            upload::cancel(&upload);
        }
        self.items.clear();
        cx.notify();
    }

    /// Uploads the files in order (`prepareForSend`), stopping at the first
    /// failure, which stays on its file.
    fn prepare_for_send(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Task<Result<Vec<Attachment>, UploadError>> {
        let order: Vec<String> = self.items.iter().map(|item| item.id.clone()).collect();
        cx.spawn(async move |this, cx| {
            let mut uploaded = Vec::new();
            for id in order {
                let started = this
                    .update(cx, |this, cx| this.start_one(&id, cx))
                    .map_err(|_| UploadError::Cancelled)?;
                let attachment = match started {
                    Start::Done(attachment) => attachment,
                    Start::Missing => return Err(UploadError::Cancelled),
                    Start::Running(task) => task.await?,
                };
                uploaded.push(attachment);
            }
            Ok(uploaded)
        })
    }

    fn start_one(&mut self, id: &str, cx: &mut Context<Self>) -> Start {
        let Some(item) = self.items.iter_mut().find(|item| item.id == id) else {
            return Start::Missing;
        };
        if let Some(attachment) = &item.uploaded {
            return Start::Done(attachment.clone());
        }
        let job = Upload::new(item.name.clone(), item.mime.clone(), item.source.clone());
        item.status = Status::Preparing;
        item.error = None;
        item.transfer = None;
        let voice = item.voice.as_ref().map(|voice| voice.duration);
        self.active.insert(id.to_string(), job.id.clone());
        cx.notify();
        let (report, mut stages) = mpsc::unbounded();
        let media = self.media.clone();
        let file = id.to_string();
        cx.spawn(async move |this, cx| {
            while let Some(stage) = stages.next().await {
                let _ = this.update(cx, |this, cx| {
                    this.replace(&file, cx, |item| match stage {
                        Stage::Preparing => item.status = Status::Preparing,
                        Stage::Uploading => item.status = Status::Uploading,
                        Stage::Sent { sent, total } if total > 0 && sent <= total => {
                            item.status = Status::Uploading;
                            item.transfer = Some((sent, total));
                        }
                        Stage::Sent { .. } => {}
                    })
                });
            }
        })
        .detach();
        let file = id.to_string();
        let upload_id = job.id.clone();
        Start::Running(cx.spawn(async move |this, cx| {
            let result = upload::send(&media, job, move |stage| {
                let _ = report.unbounded_send(stage);
            })
            .await;
            this.update(cx, |this, cx| {
                // A removed or paused file's result is no longer wanted.
                let current = this.active.get(&file) == Some(&upload_id);
                if current {
                    this.active.remove(&file);
                }
                match result {
                    Ok(mut attachment) if current => {
                        attachment.voice_duration = voice;
                        this.replace(&file, cx, |item| {
                            item.status = Status::Ready;
                            item.uploaded = Some(attachment.clone());
                            item.transfer = None;
                        });
                        Ok(attachment)
                    }
                    Ok(_) => Err(UploadError::Cancelled),
                    Err(error) => {
                        if current {
                            this.replace(&file, cx, |item| {
                                item.status = Status::Error;
                                item.error = Some(error.to_string().into());
                                item.transfer = None;
                            });
                        }
                        Err(error)
                    }
                }
            })
            .unwrap_or(Err(UploadError::Cancelled))
        }))
    }
}

enum Start {
    Done(Attachment),
    Missing,
    Running(Task<Result<Attachment, UploadError>>),
}

/// The drafts and send queues of every community, keyed by its partition.
#[derive(Default)]
struct Registry {
    drafts: HashMap<(String, String), Entity<AttachmentDraft>>,
    queues: HashMap<String, Entity<Queue>>,
}

impl Global for Registry {}

fn partition(community: &Entity<Community>, cx: &App) -> String {
    let community = community.read(cx);
    format!("{}|{}", community.origin.http(), community.session.viewer())
}

/// The draft for `key` (`draft:<channel>` or its thread's), created when
/// first asked for. A composer showing it calls [`view`] and [`leave`].
pub fn draft(community: &Entity<Community>, key: &str, cx: &mut App) -> Entity<AttachmentDraft> {
    let scope = (partition(community, cx), key.to_string());
    if let Some(draft) = cx.default_global::<Registry>().drafts.get(&scope) {
        return draft.clone();
    }
    let media = community.read(cx).session.media().clone();
    let draft = cx.new(|_| AttachmentDraft::new(media));
    cx.default_global::<Registry>()
        .drafts
        .insert(scope, draft.clone());
    draft
}

/// A composer started showing `draft`.
pub fn view(draft: &Entity<AttachmentDraft>, cx: &mut App) {
    draft.update(cx, |draft, _| draft.viewers += 1);
}

/// A composer stopped showing `draft`. With nobody left, uploads pause and
/// an empty draft is forgotten.
pub fn leave(
    community: &Entity<Community>,
    key: &str,
    draft: &Entity<AttachmentDraft>,
    cx: &mut App,
) {
    let empty = draft.update(cx, |draft, cx| {
        draft.viewers = draft.viewers.saturating_sub(1);
        if draft.viewers > 0 {
            return false;
        }
        draft.cancel(cx);
        draft.items.is_empty() && draft.recovered.is_none()
    });
    if empty {
        let scope = (partition(community, cx), key.to_string());
        let registry = cx.default_global::<Registry>();
        if registry.drafts.get(&scope) == Some(draft) {
            registry.drafts.remove(&scope);
        }
    }
}

/// Adds files to `draft`, within the community's retained-bytes budget.
pub fn add(
    community: &Entity<Community>,
    draft: &Entity<AttachmentDraft>,
    files: Vec<NewFile>,
    cx: &mut App,
) -> Result<(), String> {
    let prefix = format!("{}|", partition(community, cx));
    let retained: u64 = cx
        .default_global::<Registry>()
        .drafts
        .iter()
        .filter(|((scope, _), entity)| scope.starts_with(&prefix) && *entity != draft)
        .map(|(_, entity)| entity.clone())
        .collect::<Vec<_>>()
        .into_iter()
        .map(|entity| entity.read(cx).retained())
        .sum();
    draft.update(cx, |draft, cx| draft.add(files, retained, cx))
}

// ---------------------------------------------------------------------------
// Background sends

/// A notice of the queue: a failure, or a send whose files could not be
/// handed back.
#[derive(Clone)]
pub struct Notice {
    pub id: String,
    pub message: SharedString,
    /// A failed send kept in the queue, which "Retry failed send recovery"
    /// hands back.
    pub recovery: Option<String>,
}

/// What the pill shows (`BackgroundUploads`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Progress {
    pub phase: &'static str,
    /// Fraction of bytes sent, or `None` while there are no byte counts.
    pub fraction: Option<f64>,
}

/// One send's files and how it ends.
struct Job {
    id: String,
    owner: String,
    store: Entity<AttachmentDraft>,
    cancelled: bool,
    /// The draft text as sent, handed back on failure.
    draft: String,
    /// The draft the composer was left with, which a recovery may replace.
    followup: String,
    /// For the host, once the files are uploaded.
    ready: Option<oneshot::Sender<Vec<Attachment>>>,
}

/// A failed send waiting to be handed back.
struct Held {
    owner: String,
    files: Vec<DraftFile>,
    draft: String,
    followup: String,
    error: Option<SharedString>,
}

/// The community's background sends (`queueFor`).
pub struct Queue {
    community: WeakEntity<Community>,
    jobs: Vec<Job>,
    held: HashMap<String, Held>,
    pub notices: Vec<Notice>,
    /// Composers that can show notices; the first one does.
    hosts: Vec<EntityId>,
}

pub fn queue(community: &Entity<Community>, cx: &mut App) -> Entity<Queue> {
    let scope = partition(community, cx);
    if let Some(queue) = cx.default_global::<Registry>().queues.get(&scope) {
        return queue.clone();
    }
    let weak = community.downgrade();
    let queue = cx.new(|_| Queue {
        community: weak,
        jobs: Vec::new(),
        held: HashMap::new(),
        notices: Vec::new(),
        hosts: Vec::new(),
    });
    cx.default_global::<Registry>()
        .queues
        .insert(scope, queue.clone());
    queue
}

/// The host's handle on a background send: the uploaded files, then how
/// publishing went.
#[derive(Clone)]
pub struct BackgroundSend {
    queue: WeakEntity<Queue>,
    job: String,
    uploaded: futures::future::Shared<oneshot::Receiver<Vec<Attachment>>>,
}

impl BackgroundSend {
    /// The uploaded files, or `None` when the send failed or was cancelled;
    /// its files and text are back in the draft then.
    pub async fn uploaded(&self) -> Option<Vec<Attachment>> {
        self.uploaded.clone().await.ok()
    }

    /// Reports how publishing went. A failure hands the files back with
    /// "Message failed to send: …".
    pub fn settle(&self, result: &Result<(), String>, cx: &mut App) {
        if let Some(queue) = self.queue.upgrade() {
            queue.update(cx, |queue, cx| queue.settle(&self.job, result, cx));
        }
    }
}

impl std::fmt::Debug for BackgroundSend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackgroundSend")
            .field("job", &self.job)
            .finish()
    }
}

impl Queue {
    pub fn uploading(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// A conversation cannot send again until its send settles.
    pub fn pending(&self, owner: &str) -> bool {
        self.jobs.iter().any(|job| job.owner == owner)
    }

    pub fn progress(&self, cx: &App) -> Progress {
        let files: Vec<DraftFile> = self
            .jobs
            .iter()
            .flat_map(|job| job.store.read(cx).items.clone())
            .collect();
        let known = files
            .iter()
            .all(|item| item.status == Status::Ready || item.transfer.is_some());
        let total: u64 = files
            .iter()
            .map(|item| {
                item.uploaded
                    .as_ref()
                    .map(|a| a.size)
                    .or(item.transfer.map(|(_, total)| total))
                    .unwrap_or(0)
            })
            .sum();
        let sent: u64 = files
            .iter()
            .map(|item| match item.status {
                Status::Ready => item.uploaded.as_ref().map_or(0, |a| a.size),
                _ => item.transfer.map_or(0, |(sent, _)| sent),
            })
            .sum();
        let phase = if files.iter().any(|item| item.status == Status::Uploading) {
            "Uploading"
        } else if files.iter().all(|item| item.status == Status::Ready) {
            "Finishing"
        } else {
            "Preparing"
        };
        Progress {
            phase,
            fraction: (known && total > 0).then(|| sent as f64 / total as f64),
        }
    }

    /// Whether `host` shows the notices: only the first composer does.
    pub fn hosts_notices(&self, host: EntityId) -> bool {
        self.hosts.first() == Some(&host)
    }

    pub fn add_host(&mut self, host: EntityId, cx: &mut Context<Self>) {
        self.hosts.push(host);
        cx.notify();
    }

    pub fn remove_host(&mut self, host: EntityId, cx: &mut Context<Self>) {
        self.hosts.retain(|other| *other != host);
        cx.notify();
    }

    pub fn dismiss(&mut self, id: &str, cx: &mut Context<Self>) {
        self.notices.retain(|notice| notice.id != id);
        cx.notify();
    }

    /// Takes `files` from a draft after Send and uploads them in order.
    /// `draft` is the text as sent and `followup` what the composer was
    /// left with, for handing back.
    pub fn start(
        &mut self,
        owner: String,
        files: Vec<DraftFile>,
        draft: String,
        followup: String,
        cx: &mut Context<Self>,
    ) -> Option<BackgroundSend> {
        let community = self.community.upgrade()?;
        let id = format!("background:{}", uuid::Uuid::new_v4());
        let store = self::draft(&community, &id, cx);
        store.update(cx, |store, cx| store.adopt(files, cx));
        cx.observe(&store, |_, _, cx| cx.notify()).detach();
        let (ready, uploaded) = oneshot::channel();
        let prepared = store.update(cx, |store, cx| store.prepare_for_send(cx));
        let job_id = id.clone();
        cx.spawn(async move |this, cx| {
            let result = prepared.await;
            let _ = this.update(cx, |queue, cx| queue.uploaded(&job_id, result, cx));
        })
        .detach();
        self.jobs.push(Job {
            id: id.clone(),
            owner,
            store,
            cancelled: false,
            draft,
            followup,
            ready: Some(ready),
        });
        cx.notify();
        Some(BackgroundSend {
            queue: cx.entity().downgrade(),
            job: id,
            uploaded: futures::FutureExt::shared(uploaded),
        })
    }

    /// Cancel stops the newest send and hands it back like a failure.
    pub fn cancel_newest(&mut self, cx: &mut Context<Self>) {
        let Some(job) = self.jobs.iter_mut().rev().find(|job| !job.cancelled) else {
            return;
        };
        job.cancelled = true;
        let store = job.store.clone();
        store.update(cx, |store, cx| {
            for (_, upload) in store.active.drain() {
                upload::cancel(&upload);
            }
            cx.notify();
        });
    }

    fn uploaded(
        &mut self,
        id: &str,
        result: Result<Vec<Attachment>, UploadError>,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        match result {
            Ok(files) if !self.jobs[index].cancelled => {
                if let Some(ready) = self.jobs[index].ready.take()
                    && ready.send(files).is_ok()
                {
                    return;
                }
                // Nobody publishes it any more.
                self.finish(index, None, None, cx);
            }
            Ok(_) => self.finish(index, None, None, cx),
            Err(error) => {
                let job = &self.jobs[index];
                let notice = (!job.cancelled && error != UploadError::Cancelled)
                    .then(|| format!("Upload failed: {error}"));
                self.finish(index, notice, None, cx);
            }
        }
    }

    fn settle(&mut self, id: &str, result: &Result<(), String>, cx: &mut Context<Self>) {
        let Some(index) = self.jobs.iter().position(|job| job.id == id) else {
            return;
        };
        match result {
            Ok(()) => {
                let job = self.jobs.remove(index);
                let voices: Vec<String> = job
                    .store
                    .read(cx)
                    .items
                    .iter()
                    .filter_map(|item| item.voice.as_ref().map(|voice| voice.key.clone()))
                    .collect();
                for key in voices {
                    crate::ui::media::forget_local(&key, cx);
                }
                forget(&job.store, &job.id, self.community.upgrade(), cx);
                cx.notify();
            }
            Err(error) => self.finish(
                index,
                Some(format!("Message failed to send: {error}")),
                Some(error.clone()),
                cx,
            ),
        }
    }

    /// Ends a send that did not publish: its files and text go back to its
    /// draft, or stay here with a notice when that draft moved on. A
    /// publishing `error` shows in the composer too.
    fn finish(
        &mut self,
        index: usize,
        notice: Option<String>,
        error: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let job = self.jobs.remove(index);
        let files = job.store.read(cx).items.clone();
        forget(&job.store, &job.id, self.community.upgrade(), cx);
        let held = Held {
            owner: job.owner,
            files,
            draft: job.draft,
            followup: job.followup,
            error: error.map(SharedString::from),
        };
        if !self.restore(&held, cx) {
            self.notices.push(Notice {
                id: job.id.clone(),
                message: RECOVERY_CONFLICT.into(),
                recovery: Some(job.id.clone()),
            });
            self.held.insert(job.id.clone(), held);
        }
        if let Some(message) = notice {
            self.notices.push(Notice {
                id: format!("{}:failure", job.id),
                message: message.into(),
                recovery: None,
            });
        }
        cx.notify();
    }

    /// "Retry failed send recovery".
    pub fn retry_recovery(&mut self, id: &str, cx: &mut Context<Self>) {
        let Some(held) = self.held.remove(id) else {
            return;
        };
        if self.restore(&held, cx) {
            self.notices.retain(|notice| notice.id != id);
        } else {
            self.held.insert(id.to_string(), held);
        }
        cx.notify();
    }

    /// Hands `held` back to its draft when nothing replaced it: the saved
    /// text is still what was sent, the follow-up, or empty, and its draft
    /// has no files.
    fn restore(&self, held: &Held, cx: &mut App) -> bool {
        let Some(community) = self.community.upgrade() else {
            return false;
        };
        let target = draft(&community, &held.owner, cx);
        if !target.read(cx).items.is_empty() {
            return false;
        }
        let key = held.owner.clone();
        let partition = community.read(cx).partition();
        let store = &cx.global::<crate::community::AppState>().store;
        let saved = store.view_state(&partition, &key).ok().flatten();
        let untouched = saved.as_ref().is_none_or(|saved| {
            *saved == held.draft || *saved == held.followup || super::draft_is_empty(saved)
        });
        if !untouched {
            return false;
        }
        if store
            .set_view_state(&partition, &key, Some(&held.draft))
            .is_err()
        {
            return false;
        }
        let files = held.files.clone();
        let error = held.error.clone();
        let draft_text = held.draft.clone();
        target.update(cx, |target, cx| {
            target.adopt(files, cx);
            target.recovered = Some(Recovered {
                draft: draft_text,
                error,
            });
            cx.notify();
        });
        true
    }
}

const RECOVERY_CONFLICT: &str = "Failed send was kept because this draft changed. Clear the draft, then retry recovery before leaving this session.";

fn forget(
    store: &Entity<AttachmentDraft>,
    key: &str,
    community: Option<Entity<Community>>,
    cx: &mut App,
) {
    store.update(cx, |store, cx| {
        store.active.clear();
        store.items.clear();
        cx.notify();
    });
    if let Some(community) = community {
        let scope = (partition(&community, cx), key.to_string());
        cx.default_global::<Registry>().drafts.remove(&scope);
    }
}

/// Files read from paths: their size and type, without reading them.
pub fn files_from_paths(paths: Vec<std::path::PathBuf>) -> Vec<Result<NewFile, (String, String)>> {
    paths
        .into_iter()
        .filter(|path| path.is_file())
        .map(|path| {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| "File".into());
            let source = Source::Path(path);
            let size = source
                .len()
                .map_err(|_| (name.clone(), UploadError::Failed.to_string()))?;
            let header = source.header(64).unwrap_or_default();
            Ok(NewFile {
                mime: upload::mime_type(&name, &header),
                name,
                size,
                source,
                voice: None,
            })
        })
        .collect()
}

/// A file from memory, such as a pasted image.
pub fn file_from_bytes(name: String, bytes: Vec<u8>) -> NewFile {
    let mime = upload::mime_type(&name, &bytes);
    NewFile {
        name,
        mime,
        size: bytes.len() as u64,
        source: Source::Bytes(Arc::from(bytes)),
        voice: None,
    }
}

/// `formatSize` in `ComposerAttachments`: "N KB" (at least 1) under a MiB,
/// else "N.N MB".
pub fn file_size(bytes: u64) -> String {
    const MIB: f64 = 1024. * 1024.;
    if (bytes as f64) < MIB {
        format!("{} KB", ((bytes as f64 / 1024.).round() as u64).max(1))
    } else {
        format!("{:.1} MB", bytes as f64 / MIB)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn sizes_read_like_the_reference() {
        assert_eq!(file_size(1), "1 KB");
        assert_eq!(file_size(1536), "2 KB");
        assert_eq!(file_size(1024 * 1023), "1023 KB");
        assert_eq!(file_size(1024 * 1024), "1.0 MB");
        assert_eq!(file_size(1_258_291), "1.2 MB");
        assert_eq!(file_size(500 * 1024 * 1024), "500.0 MB");
    }

    #[test]
    fn statuses_label_like_the_reference() {
        assert_eq!(Status::Queued.label(), "Queued");
        assert_eq!(Status::Preparing.label(), "Preparing…");
        assert_eq!(Status::Uploading.label(), "Uploading…");
        assert_eq!(Status::Ready.label(), "Ready");
        assert_eq!(Status::Error.label(), "Upload failed");
        assert!(!Status::Queued.busy() && Status::Uploading.busy());
    }
}
