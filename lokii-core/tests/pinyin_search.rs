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

#[test]
fn test_pinyin_mixed_initials_and_full_search() {
    let entries = vec![
        make_entry("截图表.xlsx", "/Users/test/Docs/截图表.xlsx"),
        make_entry("截图工具", "/Users/test/Apps/截图工具"),
        make_entry("剪贴板.md", "/Users/test/Docs/剪贴板.md"),
        make_entry("北京市.pdf", "/Users/test/Docs/北京市.pdf"),
        make_entry("文件上传.zip", "/Users/test/Downloads/文件上传.zip"),
        make_entry("工作.txt", "/Users/test/Docs/工作.txt"),
        make_entry("2026年财务预算表.xlsx", "/Users/test/Docs/2026年财务预算表.xlsx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    let cases = [
        ("jietb", "截图表.xlsx", "截图表"),
        ("jiet", "截图工具", "截图"),
        ("jiantb", "剪贴板.md", "剪贴板"),
        ("jiant", "剪贴板.md", "剪贴"),
        ("beijings", "北京市.pdf", "北京市"),
        ("wenjiansc", "文件上传.zip", "文件上传"),
        ("gzuo", "工作.txt", "工作"),
        ("2026ncw", "2026年财务预算表.xlsx", "2026年财务"),
    ];

    for (query, expected_name, expected_slice) in cases {
        let results = search(&store, query, &filter, 10);
        assert!(
            results.iter().any(|r| r.entry.name == expected_name),
            "query {query:?} should match {expected_name:?}, got {:?}",
            results.iter().map(|r| &r.entry.name).collect::<Vec<_>>()
        );
        let hit = results
            .iter()
            .find(|r| r.entry.name == expected_name)
            .unwrap();
        assert!(
            !hit.match_positions.is_empty(),
            "query {query:?} on {expected_name:?} should produce match_positions"
        );
        let (s, e) = hit.match_positions[0];
        assert_eq!(
            hit.entry.name.get(s..e),
            Some(expected_slice),
            "query {query:?} highlight span ({s}, {e}) mismatch on {expected_name:?}"
        );
    }
}

#[test]
fn test_pinyin_syllable_boundary_precision() {
    let entries = vec![
        make_entry("工作汇报.docx", "/Users/test/Docs/工作汇报.docx"),
        make_entry("文件上传.zip", "/Users/test/Downloads/文件上传.zip"),
        make_entry("西安.txt", "/Users/test/Docs/西安.txt"),
        make_entry("高奥.txt", "/Users/test/Docs/高奥.txt"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    assert!(
        search(&store, "ngzu", &filter, 10).is_empty(),
        "ngzu must not cross-match inside gong+zuo"
    );
    assert!(
        search(&store, "enjian", &filter, 10).is_empty(),
        "enjian must not skip initial w of wen+jian"
    );
    assert!(
        search(&store, "ian", &filter, 10).is_empty(),
        "ian must not skip initial x of xi+an"
    );

    let ga_results = search(&store, "ga", &filter, 10);
    assert_eq!(ga_results.len(), 1);
    assert_eq!(ga_results[0].entry.name, "高奥.txt");
    assert_eq!(
        ga_results[0].match_positions,
        vec![(0, 3)],
        "ga on 高奥.txt should prefer single-syllable prefix 高 (0..3) over initials 高奥 (0..6)"
    );
}

#[test]
fn test_pinyin_v_u_equivalence_boundaries() {
    let entries = vec![
        make_entry("绿色.txt", "/Users/test/Docs/绿色.txt"),
        make_entry("女儿.png", "/Users/test/Docs/女儿.png"),
        make_entry("略过.txt", "/Users/test/Docs/略过.txt"),
        make_entry("觉得.txt", "/Users/test/Docs/觉得.txt"),
        make_entry("概率统计.pdf", "/Users/test/Docs/概率统计.pdf"),
        make_entry("乱码.txt", "/Users/test/Docs/乱码.txt"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    let positive = [
        ("lvse", "绿色.txt"),
        ("nver", "女儿.png"),
        ("lve", "略过.txt"),
        ("jve", "觉得.txt"),
        ("gailv", "概率统计.pdf"),
    ];
    for (query, expected_name) in positive {
        let results = search(&store, query, &filter, 10);
        assert!(
            results.iter().any(|r| r.entry.name == expected_name),
            "v/u query {query:?} should match {expected_name:?}"
        );
    }

    let lv_results = search(&store, "lv", &filter, 10);
    assert!(
        lv_results.iter().all(|r| r.entry.name != "乱码.txt"),
        "lv must not match luan in 乱码.txt"
    );
}

#[test]
fn test_pinyin_polyphones_simplified_and_traditional() {
    let entries = vec![
        make_entry("重量.xlsx", "/Users/test/Docs/重量.xlsx"),
        make_entry("重新整理.md", "/Users/test/Docs/重新整理.md"),
        make_entry("银行对账单.pdf", "/Users/test/Docs/银行对账单.pdf"),
        make_entry("行事历.ics", "/Users/test/Docs/行事历.ics"),
        make_entry("長度说明.txt", "/Users/test/Docs/長度说明.txt"),
        make_entry("调试日志.log", "/Users/test/Docs/调试日志.log"),
        make_entry("地图.png", "/Users/test/Docs/地图.png"),
        make_entry("音樂.mp3", "/Users/test/Media/音樂.mp3"),
        make_entry("乾坤.txt", "/Users/test/Docs/乾坤.txt"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    let cases = [
        ("zhongliang", "重量.xlsx"),
        ("chongxin", "重新整理.md"),
        ("yinhang", "银行对账单.pdf"),
        ("xingshi", "行事历.ics"),
        ("changdu", "長度说明.txt"),
        ("tiaoshi", "调试日志.log"),
        ("ditu", "地图.png"),
        ("yinyue", "音樂.mp3"),
        ("qiankun", "乾坤.txt"),
    ];

    for (query, expected_name) in cases {
        let results = search(&store, query, &filter, 10);
        assert!(
            results.iter().any(|r| r.entry.name == expected_name),
            "polyphone query {query:?} should match {expected_name:?}"
        );
        let hit = results
            .iter()
            .find(|r| r.entry.name == expected_name)
            .unwrap();
        assert!(
            !hit.match_positions.is_empty(),
            "polyphone query {query:?} should yield match_positions"
        );
    }
}

#[test]
fn test_cjk_and_pinyin_mixed_queries() {
    let entries = vec![
        make_entry("2026年财务预算表.xlsx", "/Users/test/Docs/2026年财务预算表.xlsx"),
        make_entry("工作汇报.docx", "/Users/test/Docs/工作汇报.docx"),
        make_entry("工作匯報.docx", "/Users/test/Docs/工作匯報.docx"),
    ];

    let store = IndexStore::new();
    store.replace(entries);
    let filter = CompiledRecordFilter::default();

    // 1. 多词汉字+拼音："2026年 cwysb"
    let r1 = search(&store, "2026年 cwysb", &filter, 10);
    assert_eq!(r1.len(), 1);
    assert_eq!(r1[0].entry.name, "2026年财务预算表.xlsx");
    assert_eq!(r1[0].match_positions.len(), 2);
    let s1_0 = &r1[0].entry.name[r1[0].match_positions[0].0..r1[0].match_positions[0].1];
    let s1_1 = &r1[0].entry.name[r1[0].match_positions[1].0..r1[0].match_positions[1].1];
    assert_eq!(s1_0, "2026年");
    assert_eq!(s1_1, "财务预算表");

    // 2. 多词汉字+拼音："财务 ysb"
    let r2 = search(&store, "财务 ysb", &filter, 10);
    assert_eq!(r2.len(), 1);
    assert_eq!(r2[0].entry.name, "2026年财务预算表.xlsx");
    assert_eq!(r2[0].match_positions.len(), 2);

    // 3. 单词内汉字+拼音混合："工作hb" 同时命中简体与繁体，并高亮完整 "工作汇报"/"工作匯報"
    let r3 = search(&store, "工作hb", &filter, 10);
    assert_eq!(r3.len(), 2);
    for hit in &r3 {
        assert_eq!(hit.match_positions, vec![(0, "工作汇报".len())]);
    }

    // 4. 单词内汉字+全拼："工作huibao"
    let r4 = search(&store, "工作huibao", &filter, 10);
    assert_eq!(r4.len(), 2);

    // 5. 纯汉字不匹配词依然返回空，杜绝误入拼音通道
    let r5 = search(&store, "中文", &filter, 10);
    assert!(r5.is_empty());
}
