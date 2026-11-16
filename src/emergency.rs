//! The emergency sheet: a page to print, fill in by hand and keep somewhere
//! safe. VeilForge never writes the master password into it.

use crate::util;

fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Name of the per-drive holding folder (at the top of each drive).
pub const HOLD_FOLDER: &str = "VeilForge.sys";

/// The sheet as a self-contained HTML page (no scripts, no external files).
///
/// `data_dir` is where the encrypted list of protected items lives and
/// `volumes` are the drive roots (`D:\`) that hold hidden items right now.
/// The function takes no password on purpose: there is nothing it could leak.
pub fn html(data_dir: &str, volumes: &[String], now: i64) -> String {
    let mut drives: Vec<String> = volumes
        .iter()
        .filter(|v| !v.trim().is_empty())
        .map(|v| {
            let root = v.trim_end_matches('\\');
            format!(
                "<li><span class=\"mono\">{}\\{HOLD_FOLDER}</span></li>",
                esc(root)
            )
        })
        .collect();
    drives.sort();
    drives.dedup();
    let drives = if drives.is_empty() {
        format!(
            "<p>Nothing is hidden right now. When something is, it is kept in a folder called \
             <span class=\"mono\">{HOLD_FOLDER}</span> at the top of the same drive.</p>"
        )
    } else {
        format!(
            "<p>Hidden items are kept, under random names, in these folders:</p><ul>{}</ul>\
             <p class=\"small\">Each drive has its own folder because items never leave their drive. \
             Do not rename, move or delete anything in there by hand.</p>",
            drives.join("")
        )
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en"><head><meta charset="utf-8"><title>VeilForge emergency sheet</title>
<style>
body{{font-family:"Segoe UI",Arial,sans-serif;color:#171a26;max-width:720px;margin:40px auto;padding:0 24px;line-height:1.5}}
h1{{font-size:26px;margin-bottom:4px}} h2{{font-size:16px;margin:0 0 6px}}
.sub{{color:#575d70;margin-top:0}}
.box{{border:1px solid #cfd3df;border-radius:10px;padding:16px 18px;margin:16px 0}}
.line{{border-bottom:1.5px solid #171a26;height:38px;margin:6px 0 14px}}
.mono{{font-family:Consolas,monospace;word-break:break-all}}
.warn{{border-color:#e03d31}} .small{{color:#575d70;font-size:13px}}
ol,ul{{margin:6px 0;padding-left:22px}} li{{margin:3px 0}}
@media print{{body{{margin:10mm auto}}}}
</style></head><body>
<h1>VeilForge emergency sheet</h1>
<p class="sub">Print this page, fill it in by hand, and keep it somewhere safe (a locked drawer, a safe, with someone you trust). Sheet created: {date}</p>
<div class="box"><h2>Master password</h2>
<p>Write the master password here by hand. It is not printed, on purpose.</p>
<div class="line"></div><div class="line"></div></div>
<div class="box"><h2>If VeilForge is gone (new Windows, deleted program, lost settings)</h2>
<ol>
<li>Install VeilForge again on this PC and start it.</li>
<li>If it asks you to choose a new master password, choose "Lost the app data? Rebuild from recovery copies" instead and type the master password from this sheet. VeilForge finds your protected items again.</li>
<li>Otherwise type the master password from this sheet to open VeilForge.</li>
<li>To get everything back at once, open "Health check" and choose "Panic restore". Every file and folder goes back to where it was, exactly as it was. The same is available without the window: <span class="mono">VeilForge.exe --restore-all</span></li>
</ol></div>
<div class="box"><h2>Where things are</h2>
<p>VeilForge's own list of protected items (encrypted with the master password):<br><span class="mono">{data}</span></p>
{drives}</div>
<div class="box warn"><h2>Read this</h2>
<p>There is no "forgot password". Nobody, including the developer, can make VeilForge unlock your items without the master password. Your files are not encrypted, so they are not destroyed if the password is lost, but getting them back then needs someone who knows Windows well (an administrator can take ownership of the folders above).</p>
<p>Anyone who has this sheet and this PC can unlock everything. Do not keep a photo or scan of the filled-in sheet on a computer or phone.</p></div>
<p class="small">VeilForge does not encrypt files. It protects them against normal use of this Windows account; an administrator, another operating system or a disk tool can still reach them.</p>
</body></html>
"#,
        date = util::fmt_date(now),
        data = esc(data_dir),
        drives = drives,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheet_is_escaped_and_holds_no_password() {
        let page = html(
            "C:\\Users\\a<b>\\AppData\\Roaming\\VeilForge",
            &["D:\\".to_string(), "C:\\".to_string(), "D:\\".to_string()],
            0,
        );
        assert!(page.contains("a&lt;b&gt;"));
        assert!(!page.contains("a<b>"));
        assert!(!page.contains("<script"));
        assert!(!page.contains("<input"));
        assert_eq!(page.matches("VeilForge.sys</span></li>").count(), 2);
        assert!(page.contains("C:\\VeilForge.sys"));
        assert!(page.contains("D:\\VeilForge.sys"));
        assert!(page.contains("Panic restore"));
        assert!(page.contains("--restore-all"));
        assert!(page.contains("Rebuild from recovery copies"));
        // The password is written by hand: two blank lines, and the function
        // has no parameter a password could come in through.
        assert_eq!(page.matches("class=\"line\"").count(), 2);
        let empty = html("X", &[], 0);
        assert!(empty.contains("Nothing is hidden right now"));
    }
}
