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
}
