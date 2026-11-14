//! The modal dialogs a `MapViewer` can have open, and the requests to open one.

use osm_gpui::ui::{
    about_dialog::AboutDialog, custom_imagery_dialog::CustomImageryDialog,
    delete_layer_dialog::DeleteLayerDialog, find_dialog::FindDialog, notice_dialog::NoticeDialog,
    nsi_dialog::NsiPresetDialog, parallel_dialog::ParallelDialog,
    preset_picker_dialog::PresetPickerDialog, quit_confirm_dialog::QuitConfirmDialog,
    relation_picker_dialog::RelationPickerDialog, restore_dialog::RestoreDialog,
    sign_in_dialog::SignInDialog, upload_dialog::UploadDialog,
};

/// Each dialog is `Some` while it is open. `MapViewer::open_modals` lists them
/// all in mount order.
#[derive(Default)]
pub(crate) struct Dialogs {
    /// Active custom imagery dialog, if open.
    pub(crate) custom_imagery_dialog: Option<gpui::Entity<CustomImageryDialog>>,
    /// Active "unsaved changes" quit-confirmation dialog, if open.
    pub(crate) quit_confirm_dialog: Option<gpui::Entity<QuitConfirmDialog>>,
    pub(crate) delete_layer_dialog: Option<gpui::Entity<DeleteLayerDialog>>,
    /// The startup offer to restore autosaved layers, if showing.
    pub(crate) restore_dialog: Option<gpui::Entity<RestoreDialog>>,
    /// Edit > Find, while open.
    pub(crate) find_dialog: Option<gpui::Entity<FindDialog>>,
    pub(crate) about_dialog: Option<gpui::Entity<AboutDialog>>,
    /// Active upload-review dialog, if open.
    pub(crate) upload_dialog: Option<gpui::Entity<UploadDialog>>,
    pub(crate) sign_in_dialog: Option<gpui::Entity<SignInDialog>>,
    /// Active "can't do that" notice (an upload refused for a server size
    /// limit), if open.
    pub(crate) notice_dialog: Option<gpui::Entity<NoticeDialog>>,
    /// Set by "Add to Relation…"; `render` opens the picker (it needs the
    /// window) and clears it.
    pub(crate) pending_relation_picker: bool,
    /// Active "add to relation" picker dialog, if open.
    pub(crate) relation_picker_dialog: Option<gpui::Entity<RelationPickerDialog>>,
    /// Set by "Create Parallel Way…"; `render` opens the dialog and clears it.
    pub(crate) pending_parallel_dialog: bool,
    /// Active "create parallel way" dialog, if open.
    pub(crate) parallel_dialog: Option<gpui::Entity<ParallelDialog>>,
    /// What the parallel-way dialog was last submitted with, offered again
    /// next time (this session only).
    pub(crate) parallel_params: osm_gpui::parallel::ParallelParams,
    /// Active NSI preset search dialog, if open.
    pub(crate) nsi_dialog: Option<gpui::Entity<NsiPresetDialog>>,
    /// Active "change feature type" preset picker dialog, if open.
    pub(crate) preset_picker_dialog: Option<gpui::Entity<PresetPickerDialog>>,
}
