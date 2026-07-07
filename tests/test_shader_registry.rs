#[cfg(test)]
mod shader_registry_tests {
    use hitbox_editor::shader_registry::{audit_ptcl, hash_bnsh_key, ShaderRegistry};

    #[test]
    fn test_shader_registry_dedup() {
        let mut reg = ShaderRegistry::default();
        let k1 = reg.register(vec![1, 2, 3, 4]);
        let k2 = reg.register(vec![1, 2, 3, 4]);
        assert_eq!(k1, k2);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn test_hash_bnsh_key_stable() {
        let data = b"test_bnsh_bytes";
        assert_eq!(hash_bnsh_key(data), hash_bnsh_key(data));
    }

    #[test]
    fn test_audit_empty_ptcl() {
        let ptcl = hitbox_editor::effects::PtclFile::default();
        let report = audit_ptcl(&ptcl);
        assert_eq!(report.emitters_total, 0);
    }

    #[test]
    fn test_shader_registry_merge_from() {
        let mut a = ShaderRegistry::default();
        let k_a = a.register(vec![1, 2, 3]);
        let mut b = ShaderRegistry::default();
        let k_b = b.register(vec![4, 5, 6]);
        a.merge_from(&b);
        assert_eq!(a.len(), 2);
        assert!(a.get(k_a).is_some());
        assert!(a.get(k_b).is_some());
        b.register(vec![1, 2, 3]);
        a.merge_from(&b);
        assert_eq!(a.len(), 2);
    }
}
