ALTER TABLE crates_categories ADD CONSTRAINT fk_crates_categories_category_id FOREIGN KEY (category_id) REFERENCES categories (id) ON DELETE CASCADE;