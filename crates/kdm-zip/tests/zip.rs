mod common;

use common::{noise, Item, MemSource, TempDir, ZipBuilder};
use kdm_zip::{extract, list, safe_join, Entry};
use std::io::Write;

fn text(n: usize) -> Vec<u8> {
    "Karadelik indirme yöneticisi. ".repeat(n).into_bytes()
}

fn sample() -> ZipBuilder {
    ZipBuilder::default()
        .add(Item::new("okubeni.txt", b"merhaba dunya\n", 0))
        .add(Item::new("klasor/", b"", 0))
        .add(Item::new("klasor/metin.txt", &text(500), 8))
        .add(Item::new("türkçe ğüşiöç.txt", "içerik ŞĞÜ".as_bytes(), 8))
        .add(Item::raw_name(b"eski \x81ber.txt", b"cp437", 0))
}

/// Tüm girdileri safe_join ile çıkarıp içeriği karşılaştırır.
fn extract_all(src: &MemSource, entries: &[Entry], expect: &[(&str, Vec<u8>)]) {
    let dir = TempDir::new("all");
    for e in entries {
        let out = safe_join(dir.path(), &e.name).expect("güvenli ad");
        extract(src, e, &out).unwrap();
    }
    for (name, data) in expect {
        let got = std::fs::read(safe_join(dir.path(), name).unwrap()).unwrap();
        assert_eq!(&got, data, "{name}");
    }
}

#[test]
fn lists_and_extracts_stored_and_deflate() {
    let src = MemSource::new(sample().build());
    let entries = list(&src).unwrap();
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["okubeni.txt", "klasor/", "klasor/metin.txt", "türkçe ğüşiöç.txt", "eski über.txt"]);
    assert_eq!(entries.iter().map(|e| e.index).collect::<Vec<_>>(), [0, 1, 2, 3, 4]);
    assert!(entries[1].is_dir && !entries[0].is_dir);
    assert_eq!(entries[2].method, 8);
    assert_eq!(entries[2].size, text(500).len() as u64);
    assert!(entries[2].compressed < entries[2].size / 10);
    assert_eq!(entries[0].crc32, crc32fast::hash(b"merhaba dunya\n"));
    assert!(entries.iter().all(|e| !e.encrypted));

    extract_all(
        &src,
        &entries,
        &[
            ("okubeni.txt", b"merhaba dunya\n".to_vec()),
            ("klasor/metin.txt", text(500)),
            ("türkçe ğüşiöç.txt", "içerik ŞĞÜ".as_bytes().to_vec()),
            ("eski über.txt", b"cp437".to_vec()),
        ],
    );
}

#[test]
fn crc_mismatch_detected_and_no_file_left() {
    let mut item = Item::new("bozuk.txt", &text(50), 8);
    item.crc_override = Some(0xDEAD_BEEF);
    let src = MemSource::new(ZipBuilder::default().add(item).build());
    let e = &list(&src).unwrap()[0];
    let dir = TempDir::new("crc");
    let out = dir.path().join("bozuk.txt");
    let err = extract(&src, e, &out).unwrap_err().to_string();
    assert!(err.contains("CRC32"), "{err}");
    assert!(!out.exists());
    assert!(!dir.path().join("bozuk.txt.part").exists());

    // stored yöntemde de
    let mut item = Item::new("ham.bin", b"veri", 0);
    item.crc_override = Some(1);
    let src = MemSource::new(ZipBuilder::default().add(item).build());
    let e = &list(&src).unwrap()[0];
    assert!(extract(&src, e, &dir.path().join("ham.bin")).unwrap_err().to_string().contains("CRC32"));
}

#[test]
fn understated_size_is_rejected() {
    // Merkezi dizin 10 bayt diyor, deflate çok daha fazlasını üretiyor
    let mut item = Item::new("bomba.txt", &text(1000), 8);
    item.size_override = Some(10);
    let src = MemSource::new(ZipBuilder::default().add(item).build());
    let e = &list(&src).unwrap()[0];
    let dir = TempDir::new("bomb");
    let out = dir.path().join("bomba.txt");
    assert!(extract(&src, e, &out).is_err());
    assert!(!out.exists());
}

#[test]
fn zip64_archive_lists_and_extracts() {
    let mut b = sample();
    b.zip64 = true;
    let data = b.build();
    let src = MemSource::new(data);
    let entries = list(&src).unwrap();
    assert_eq!(entries.len(), 5);
    assert_eq!(entries[2].size, text(500).len() as u64);
    extract_all(&src, &entries, &[("klasor/metin.txt", text(500)), ("okubeni.txt", b"merhaba dunya\n".to_vec())]);
}

#[test]
fn eocd_found_with_archive_comment() {
    let fake = [b"yorumda sahte PK\x05\x06 imza".as_slice(), &[0; 40]].concat();
    for comment in [b"kisa yorum".to_vec(), fake, vec![b'x'; 65535]] {
        let mut b = sample();
        b.comment = comment;
        let src = MemSource::new(b.build());
        let entries = list(&src).unwrap();
        assert_eq!(entries.len(), 5);
        extract_all(&src, &entries, &[("klasor/metin.txt", text(500))]);
    }
}

#[test]
fn prepended_data_offsets_corrected() {
    // Kendiliğinden açılan (SFX) arşiv gibi: ZIP'in önünde başka veri var, ofsetler ona göre değil
    for zip64 in [false, true] {
        let mut b = sample();
        b.prefix = noise(5000, 9);
        b.zip64 = zip64;
        let src = MemSource::new(b.build());
        let entries = list(&src).unwrap();
        assert_eq!(entries[0].local_offset, 5000);
        extract_all(&src, &entries, &[("klasor/metin.txt", text(500)), ("eski über.txt", b"cp437".to_vec())]);
    }
}

#[test]
fn reads_are_few_and_small() {
    // ~4 MiB sıkışmayan veri: listeleme yalnız sondan okumalı
    let mut b = ZipBuilder::default();
    for i in 0..40 {
        b = b.add(Item::new(&format!("veri/{i:02}.bin"), &noise(100 * 1024, i + 1), 0));
    }
    let src = MemSource::new(b.build());
    let len = src.data.len() as u64;
    let entries = list(&src).unwrap();
    assert_eq!(entries.len(), 40);
    assert!(src.calls() <= 2, "{} okuma", src.calls());
    assert!(src.bytes() <= 70 * 1024, "{} bayt okundu", src.bytes());
    assert!(src.bytes() * 50 < len);

    // Tek girdi çıkarma yalnız o girdinin aralığını okur
    src.reset();
    let dir = TempDir::new("reads");
    let e = &entries[17];
    extract(&src, e, &dir.path().join("x.bin")).unwrap();
    assert_eq!(std::fs::read(dir.path().join("x.bin")).unwrap(), noise(100 * 1024, 18));
    assert!(src.calls() <= 3, "{} okuma", src.calls());
    assert!(src.bytes() <= e.compressed + 30, "{} bayt", src.bytes());
}

#[test]
fn large_entry_streamed_in_chunks() {
    // 3 MiB'lık girdi 1 MiB'lık parçalarla okunur, tek dev okuma yapılmaz
    let data = noise(3 * 1024 * 1024 + 123, 77);
    let src = MemSource::new(ZipBuilder::default().add(Item::new("buyuk.bin", &data, 8)).build());
    let e = &list(&src).unwrap()[0];
    src.reset();
    let dir = TempDir::new("big");
    extract(&src, e, &dir.path().join("buyuk.bin")).unwrap();
    assert_eq!(std::fs::read(dir.path().join("buyuk.bin")).unwrap(), data);
    assert!(src.calls() >= 4 && src.calls() <= 6, "{} okuma", src.calls());
}

#[test]
fn encrypted_and_unsupported_methods_rejected() {
    let mut enc = Item::new("gizli.txt", b"x", 0);
    enc.flags |= 1;
    let src = MemSource::new(ZipBuilder::default().add(enc).add(Item::new("lzma.bin", b"y", 14)).build());
    let entries = list(&src).unwrap();
    assert!(entries[0].encrypted);
    let dir = TempDir::new("enc");
    let err = extract(&src, &entries[0], &dir.path().join("a")).unwrap_err().to_string();
    assert!(err.contains("şifreli"), "{err}");
    let err = extract(&src, &entries[1], &dir.path().join("b")).unwrap_err().to_string();
    assert!(err.contains("LZMA"), "{err}");
}

#[test]
fn not_a_zip_or_truncated() {
    assert!(list(&MemSource::new(b"kisa".to_vec())).is_err());
    assert!(list(&MemSource::new(noise(100_000, 3))).is_err());
    let data = sample().build();
    // Baştan kesilmiş: merkezi dizin kayıtlı ofsetten önce bitiyor
    let cut = data[data.len() - 200..].to_vec();
    assert!(list(&MemSource::new(cut)).is_err());
}

#[test]
fn empty_archive() {
    let src = MemSource::new(ZipBuilder::default().build());
    assert!(list(&src).unwrap().is_empty());
}

#[test]
fn interop_with_zip_crate() {
    use zip::write::SimpleFileOptions;
    use zip::CompressionMethod;
    let mut cur = std::io::Cursor::new(Vec::new());
    {
        let mut w = zip::ZipWriter::new(&mut cur);
        let stored = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        w.start_file("a.txt", stored).unwrap();
        w.write_all(b"stored").unwrap();
        w.add_directory("alt/", stored).unwrap();
        w.start_file("alt/ğüşİ.txt", deflated).unwrap();
        w.write_all(&text(300)).unwrap();
        // large_file: ZIP64 ek alanını küçük dosyada da zorlar
        w.start_file("alt/z64.bin", deflated.large_file(true)).unwrap();
        w.write_all(&text(100)).unwrap();
        w.set_comment("arşiv yorumu").unwrap();
        w.finish().unwrap();
    }
    let src = MemSource::new(cur.into_inner());
    let entries = list(&src).unwrap();
    let names: Vec<_> = entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, ["a.txt", "alt/", "alt/ğüşİ.txt", "alt/z64.bin"]);
    assert!(entries[1].is_dir);
    extract_all(
        &src,
        &entries,
        &[("a.txt", b"stored".to_vec()), ("alt/ğüşİ.txt", text(300)), ("alt/z64.bin", text(100))],
    );
}
