//! Reading a document by its headings, for `read_file`.
//!
//! A long procedure is read a section at a time, not a window at a time: `headings` lists the
//! outline (each heading with its line number and how long its section is), `before` and `after`
//! add lines of context around each heading the way `grep -B` and `-A` do, and `section` (or
//! `path#Heading` in the path) reads one section whole. A heading is a markdown `#` line outside
//! a fenced code block, so the `# comment` lines of a shell snippet are not headings. A section
//! runs to the next heading of the same or a higher level, subsections included.

/// One heading of a document.
pub(super) struct Heading {
    /// 1-based line number.
    pub(super) line: usize,
    /// `#` count: 1 for a title, 2 for a chapter, ...
    pub(super) level: usize,
    pub(super) text: String,
}

/// The headings of `lines`, in order.
pub(super) fn headings_of(lines: &[&str]) -> Vec<Heading> {
    let mut found = Vec::new();
    // The fence that is open, if any: its character and length.
    let mut fence: Option<(char, usize)> = None;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        // A fence is up to three spaces in, then three or more backticks or tildes.
        if line.len() - trimmed.len() <= 3 {
            if let Some(mark) = trimmed.chars().next().filter(|c| matches!(*c, '`' | '~')) {
                let run = trimmed.chars().take_while(|c| *c == mark).count();
                if run >= 3 {
                    match fence {
                        None => fence = Some((mark, run)),
                        Some((open, length)) if open == mark && run >= length => fence = None,
                        Some(_) => {}
                    }
                    continue;
                }
            }
        }
        if fence.is_some() || line.len() - trimmed.len() > 3 {
            continue;
        }
        let level = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&level) && trimmed[level..].starts_with([' ', '\t']) {
            let text = trimmed[level..].trim().trim_end_matches('#').trim().to_string();
            if !text.is_empty() {
                found.push(Heading {
                    line: index + 1,
                    level,
                    text,
                });
            }
        }
    }
    found
}

/// The lines of heading `index`'s section, 1-based and inclusive: from its heading to the line
/// before the next heading of the same or a higher level.
fn section_lines(headings: &[Heading], index: usize, total: usize) -> (usize, usize) {
    let start = headings[index].line;
    let end = headings[index + 1..]
        .iter()
        .find(|next| next.level <= headings[index].level)
        .map_or(total, |next| next.line - 1);
    (start, end)
}

fn normalize(text: &str) -> String {
    text.trim()
        .trim_start_matches('#')
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The heading `selector` names: `L140` (the heading on that line), else its text — exactly,
/// then as a start ("3.7" matches "3.7 Dump" but not "3.7b Full dump"), then anywhere in it.
/// An error lists the candidates when it names none or several.
pub(super) fn find_heading(headings: &[Heading], selector: &str) -> Result<usize, String> {
    let wanted = normalize(selector);
    if let Some(number) = wanted
        .strip_prefix('l')
        .and_then(|rest| rest.parse::<usize>().ok())
    {
        if let Some(index) = headings.iter().position(|heading| heading.line == number) {
            return Ok(index);
        }
    }
    let texts: Vec<String> = headings.iter().map(|heading| normalize(&heading.text)).collect();
    let starts = |text: &String| {
        text.strip_prefix(&wanted)
            .is_some_and(|rest| !rest.starts_with(|c: char| c.is_alphanumeric()))
    };
    let tiers: [Vec<usize>; 3] = [
        (0..texts.len()).filter(|&i| texts[i] == wanted).collect(),
        (0..texts.len()).filter(|&i| starts(&texts[i])).collect(),
        (0..texts.len()).filter(|&i| texts[i].contains(&wanted)).collect(),
    ];
    for tier in &tiers {
        match tier.as_slice() {
            [] => continue,
            [only] => return Ok(*only),
            several => {
                let listed: Vec<String> = several
                    .iter()
                    .take(10)
                    .map(|&i| format!("L{}| {} {}", headings[i].line, "#".repeat(headings[i].level), headings[i].text))
                    .collect();
                return Err(format!(
                    "{} headings match {selector:?}; name one by its line, e.g. section=\"L{}\":\n{}",
                    several.len(),
                    headings[several[0]].line,
                    listed.join("\n")
                ));
            }
        }
    }
    Err(format!(
        "No heading matches {selector:?}. Call read_file with headings=true to list them."
    ))
}

/// `lines[from..=to]` (1-based, inclusive) as numbered lines.
fn numbered(lines: &[&str], from: usize, to: usize) -> String {
    (from..=to)
        .map(|number| format!("L{number}| {}\n", lines[number - 1]))
        .collect()
}

/// Cuts `body` to whole lines within `window_bytes`. Returns it and how many lines were cut off
/// the end.
fn fit(body: &str, window_bytes: usize) -> (String, usize) {
    if body.len() <= window_bytes {
        return (body.to_string(), 0);
    }
    let mut kept = 0;
    for line in body.split_inclusive('\n') {
        if kept + line.len() > window_bytes && kept > 0 {
            break;
        }
        kept += line.len();
    }
    // A first line longer than the window: its start.
    if kept > window_bytes {
        kept = window_bytes;
        while !body.is_char_boundary(kept) {
            kept -= 1;
        }
    }
    (body[..kept].to_string(), body[kept..].lines().count())
}

/// The outline of a document: every heading with its line and the length of its section. With
/// `before` / `after`, each heading comes with that many lines around it instead, like `grep -B -A`
/// (overlapping ones merge; blocks are separated by `--`). `skip` skips headings, to continue a
/// long outline.
pub(super) fn outline(
    path: &str,
    lines: &[&str],
    before: usize,
    after: usize,
    skip: usize,
    window_bytes: usize,
) -> String {
    let headings = headings_of(lines);
    let total = lines.len();
    if headings.is_empty() {
        return format!(
            "{path} has no markdown headings ({total} lines). Read it by lines with offset/limit."
        );
    }
    if skip >= headings.len() {
        return format!("{path} has {} headings; skip {skip} is past the end.", headings.len());
    }
    let mut body = String::new();
    if before == 0 && after == 0 {
        for (index, heading) in headings.iter().enumerate().skip(skip) {
            let (start, end) = section_lines(&headings, index, total);
            body.push_str(&format!(
                "L{}| {} {}  [{} lines]\n",
                heading.line,
                "#".repeat(heading.level),
                heading.text,
                end - start + 1
            ));
        }
    } else {
        // The blocks of context, merged where they touch.
        let mut blocks: Vec<(usize, usize)> = Vec::new();
        for heading in headings.iter().skip(skip) {
            let from = heading.line.saturating_sub(before).max(1);
            let to = (heading.line + after).min(total);
            match blocks.last_mut() {
                Some(last) if from <= last.1 + 1 => last.1 = last.1.max(to),
                _ => blocks.push((from, to)),
            }
        }
        for (index, (from, to)) in blocks.iter().enumerate() {
            if index > 0 {
                body.push_str("--\n");
            }
            body.push_str(&numbered(lines, *from, *to));
        }
    }
    let (body, cut) = fit(&body, window_bytes);
    let mut text = format!(
        "{path}: {} headings, {total} lines (each line starts with its number, `L<n>| `, which is \
         not part of the file)\n{body}",
        headings.len()
    );
    if cut > 0 {
        let shown = body.lines().filter(|line| !line.starts_with("--")).count();
        text.push_str(&format!(
            "\n[{cut} more line(s) of outline. Continue with headings=true and skip={}{}.]",
            skip + if before == 0 && after == 0 { shown } else { 0 },
            if before == 0 && after == 0 { String::new() } else { format!(" before={before} after={after}") }
        ));
    }
    text
}

/// One section whole: from its heading to the next of the same or a higher level. `before` and
/// `after` add lines of context outside it. `offset` skips lines of what is returned and `limit`
/// caps them, to page through a section longer than a window.
#[allow(clippy::too_many_arguments)]
pub(super) fn section(
    path: &str,
    lines: &[&str],
    selector: &str,
    before: usize,
    after: usize,
    offset: usize,
    limit: Option<usize>,
    window_bytes: usize,
) -> Result<String, String> {
    let headings = headings_of(lines);
    if headings.is_empty() {
        return Err(format!(
            "{path} has no markdown headings, so it has no sections. Read it by lines with offset/limit."
        ));
    }
    let total = lines.len();
    let index = find_heading(&headings, selector)?;
    let (start, end) = section_lines(&headings, index, total);
    let from = start.saturating_sub(before).max(1);
    let to = (end + after).min(total);
    let count = to - from + 1;
    if offset >= count {
        return Ok(format!(
            "The section {:?} of {path} has {count} line(s); offset {offset} is past the end.",
            headings[index].text
        ));
    }
    let first = from + offset;
    let last = limit.map_or(to, |limit| (first + limit.max(1) - 1).min(to));
    let (body, cut) = fit(&numbered(lines, first, last), window_bytes);
    let shown = body.lines().count();
    let mut text = format!(
        "{path}, section {:?} (lines {from}-{to} of {total}; each line starts with its number, \
         `L<n>| `, which is not part of the file)\n{body}",
        headings[index].text
    );
    let remaining = (to - last) + cut;
    if remaining > 0 {
        text.push_str(&format!(
            "\n[{remaining} more line(s) of this section. To continue, call read_file with \
             section=\"L{}\" offset={}.]",
            headings[index].line,
            offset + shown
        ));
    }
    Ok(text)
}

/// Splits `path#Heading` (or `path:Heading` when the whole string names no file) into the file and
/// the heading. `exists` says whether a candidate path is a file.
pub(super) fn split_section_path(spec: &str, exists: impl Fn(&str) -> bool) -> (String, Option<String>) {
    if exists(spec) {
        return (spec.to_string(), None);
    }
    for separator in ['#', ':'] {
        if let Some((file, heading)) = spec.rsplit_once(separator) {
            if !file.is_empty() && !heading.trim().is_empty() && exists(file) {
                return (file.to_string(), Some(heading.trim().to_string()));
            }
        }
    }
    (spec.to_string(), None)
}
