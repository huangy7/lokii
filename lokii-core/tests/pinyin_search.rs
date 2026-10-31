use lokii_core::indexer::{FileEntry, IndexStore};
use lokii_core::search::filter::CompiledRecordFilter;
use lokii_core::search::substring::search;
use lokii_core::search::wildcard;

fn make_entry(name: &str, path: &str) -> FileEntry {
    make_entry_with_modified(name, path, 1700000000)
}

fn make_entry_with_modified(name: &str, path: &str, modified: u64) -> FileEntry {
    FileEntry {
        name: name.to_string(),
        path: path.to_string(),
        is_dir: false,
        size: 1024,
        modified,
        created_at: 0,
        extension: std::path::Path::new(name)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase(),
    }
}

#[test]
fn test_pinyin_initials_and_full_search() {
    let entries = vec![
        make_entry("工作汇报.docx", "/Users/test/Documents/工作汇报.docx"),
        make_entry("2026年财务预算表.xlsx", "/Users/test/Documents/2026年财务预算表.xlsx"),
        make_entry("发票-张三.pdf", "/Users/test/Downloads/发票-张三.pdf"),
        make_entry("开发规范.md", "/Users/test/Projects/开发规范.md"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 首字母匹配：gzhb 命中 工作汇报.docx
    let results = search(&store, "gzhb", &filter, 10);
    assert!(!results.is_empty(), "gzhb should match 工作汇报.docx");
    assert_eq!(results[0].entry.name, "工作汇报.docx");
    assert!(!results[0].match_positions.is_empty());

    // 2. 全拼匹配：gongzuo 命中 工作汇报.docx
    let results = search(&store, "gongzuo", &filter, 10);
    assert!(!results.is_empty(), "gongzuo should match 工作汇报.docx");
    assert_eq!(results[0].entry.name, "工作汇报.docx");

    // 3. 混合首拼：cwysb 命中 2026年财务预算表.xlsx
    let results = search(&store, "cwysb", &filter, 10);
    assert!(!results.is_empty(), "cwysb should match 2026年财务预算表.xlsx");
    assert_eq!(results[0].entry.name, "2026年财务预算表.xlsx");

    // 4. 全拼部分匹配：fapiao 命中 发票-张三.pdf
    let results = search(&store, "fapiao", &filter, 10);
    assert!(!results.is_empty(), "fapiao should match 发票-张三.pdf");
    assert_eq!(results[0].entry.name, "发票-张三.pdf");

    // 5. 首字母部分匹配：kfgf 命中 开发规范.md
    let results = search(&store, "kfgf", &filter, 10);
    assert!(!results.is_empty(), "kfgf should match 开发规范.md");
    assert_eq!(results[0].entry.name, "开发规范.md");
}

#[test]
fn test_pinyin_multibyte_punctuation_safety() {
    let entries = vec![
        make_entry("【正式】工作总结.docx", "/Users/test/Documents/【正式】工作总结.docx"),
        make_entry("·项目进度·.xlsx", "/Users/test/Documents/·项目进度·.xlsx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 验证包含多字节非 ASCII 标点符号的文件名在拼音首字母匹配下不发生越界 panic
    let results = search(&store, "zs", &filter, 10);
    assert!(!results.is_empty(), "zs should match 【正式】工作总结.docx");
    assert_eq!(results[0].entry.name, "【正式】工作总结.docx");
    assert_eq!(results[0].match_positions.len(), 1);

    // 验证双词查询匹配：标点两侧的拼音首字母同时命中
    let results_multi = search(&store, "zs gzzj", &filter, 10);
    assert!(!results_multi.is_empty(), "zs gzzj should match 【正式】工作总结.docx");
    assert_eq!(results_multi[0].entry.name, "【正式】工作总结.docx");
    assert_eq!(results_multi[0].match_positions.len(), 2);

    // 验证全拼匹配在多字节标点场景下的正确性
    let results_full = search(&store, "zhengshi", &filter, 10);
    assert!(!results_full.is_empty(), "zhengshi should match 【正式】工作总结.docx");
    assert_eq!(results_full[0].entry.name, "【正式】工作总结.docx");
}

#[test]
fn test_mixed_query_highlighting() {
    let entries = vec![
        make_entry("2026年财务预算表.xlsx", "/Users/test/Documents/2026年财务预算表.xlsx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 混合查询：包含直接匹配词 "2026" 和拼音首字母词 "cwysb"
    let results = search(&store, "2026 cwysb", &filter, 10);
    assert!(!results.is_empty(), "mixed query should match");
    assert_eq!(results[0].entry.name, "2026年财务预算表.xlsx");
    // 两个词均需产生高亮区间
    assert_eq!(results[0].match_positions.len(), 2, "both terms should have match positions");
}

#[test]
fn test_signature_pruning_non_letter_queries() {
    let entries = vec![
        make_entry("财务预算表.xlsx", "/Users/test/Documents/财务预算表.xlsx"),
        make_entry("说明文档.docx", "/Users/test/Documents/说明文档.docx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 查询 123（无字母），不应误命中无数字的 CJK 文件
    let results = search(&store, "123", &filter, 10);
    assert!(results.is_empty(), "123 should not match 财务预算表.xlsx");

    // 查询 .pdf，不应误命中扩展名为 .xlsx / .docx 的 CJK 文件
    let results_pdf = search(&store, ".pdf", &filter, 10);
    assert!(results_pdf.is_empty(), ".pdf should not match .xlsx/.docx files");
}

#[test]
fn test_wildcard_star_recent_modified_order() {
    let entries = vec![
        make_entry_with_modified("z_old.txt", "/z_old.txt", 100),
        make_entry_with_modified("a_recent.txt", "/a_recent.txt", 300),
        make_entry_with_modified("m_mid.txt", "/m_mid.txt", 200),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    let results = wildcard::search(&store, "*", &filter, 10);
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].entry.name, "a_recent.txt");
    assert_eq!(results[1].entry.name, "m_mid.txt");
    assert_eq!(results[2].entry.name, "z_old.txt");
}
