//! Month and weekday names by locale, for `strptime(fmt; LOCALE)`
//! and `strftime(fmt; LOCALE)`: the `%B`, `%b`, `%A`, `%a` names
//! read and write a locale's forms. The tables are CLDR's, in
//! every form the data lists — a date's form (`октября`, the
//! genitive after a day number) and the stand-alone (`октябрь`),
//! wide and abbreviated — and all of them parse; writing uses the
//! form a date takes. Without a locale the fixed English names,
//! as before. An unknown locale has no table. The locales shipped,
//! by size of language: en, zh, es, ar, pt, ru, ja, de, fr, tr, it.

/// One locale's names. Of each entry's spellings the first is the
/// date form (what `strftime` writes where the format carries a
/// day), the second the stand-alone form (written where it does
/// not; the same word where the language makes no distinction),
/// and the rest further spellings; every spelling parses.
#[derive(Debug)]
pub struct Names {
    /// `%B`: a month's wide forms, the date form first.
    pub months: [&'static [&'static str]; 12],
    /// `%b`: the abbreviations, the date form first.
    pub months_abbr: [&'static [&'static str]; 12],
    /// `%A`: the weekdays from Monday.
    pub days: [&'static [&'static str]; 7],
    /// `%a`.
    pub days_abbr: [&'static [&'static str]; 7],
    /// `%p`: the day periods, CLDR's abbreviated format forms (AM
    /// and PM where the language keeps them), the Latin AM/PM read
    /// everywhere besides.
    pub periods: [&'static [&'static str]; 2],
}

macro_rules! names {
    ($($entry:expr),* $(,)?) => { [$(&$entry),*] };
}

pub static EN: Names = Names {
    months: names![
        ["January"],
        ["February"],
        ["March"],
        ["April"],
        ["May"],
        ["June"],
        ["July"],
        ["August"],
        ["September"],
        ["October"],
        ["November"],
        ["December"],
    ],
    months_abbr: names![
        ["Jan"],
        ["Feb"],
        ["Mar"],
        ["Apr"],
        ["May"],
        ["Jun"],
        ["Jul"],
        ["Aug"],
        ["Sep", "Sep", "Sept"],
        ["Oct"],
        ["Nov"],
        ["Dec"],
    ],
    days: names![
        ["Monday"],
        ["Tuesday"],
        ["Wednesday"],
        ["Thursday"],
        ["Friday"],
        ["Saturday"],
        ["Sunday"],
    ],
    days_abbr: names![
        ["Mon"],
        ["Tue"],
        ["Wed"],
        ["Thu"],
        ["Fri"],
        ["Sat"],
        ["Sun"]
    ],
    periods: names![["AM"], ["PM"]],
};

pub static RU: Names = Names {
    months: names![
        ["января", "январь"],
        ["февраля", "февраль"],
        ["марта", "март"],
        ["апреля", "апрель"],
        ["мая", "май"],
        ["июня", "июнь"],
        ["июля", "июль"],
        ["августа", "август"],
        ["сентября", "сентябрь"],
        ["октября", "октябрь"],
        ["ноября", "ноябрь"],
        ["декабря", "декабрь"],
    ],
    months_abbr: names![
        ["янв.", "янв.", "янв"],
        ["февр.", "февр.", "февр", "фев"],
        ["мар.", "март", "мар"],
        ["апр.", "апр.", "апр"],
        ["мая", "май"],
        ["июн.", "июнь", "июн"],
        ["июл.", "июль", "июл"],
        ["авг.", "авг.", "авг"],
        ["сент.", "сент.", "сент", "сен"],
        ["окт.", "окт.", "окт"],
        ["нояб.", "нояб.", "нояб", "ноя"],
        ["дек.", "дек.", "дек"],
    ],
    days: names![
        ["понедельник"],
        ["вторник"],
        ["среда"],
        ["четверг"],
        ["пятница"],
        ["суббота"],
        ["воскресенье"],
    ],
    days_abbr: names![["пн"], ["вт"], ["ср"], ["чт"], ["пт"], ["сб"], ["вс"]],
    periods: names![["AM"], ["PM"]],
};

pub static DE: Names = Names {
    months: names![
        ["Januar"],
        ["Februar"],
        ["März"],
        ["April"],
        ["Mai"],
        ["Juni"],
        ["Juli"],
        ["August"],
        ["September"],
        ["Oktober"],
        ["November"],
        ["Dezember"],
    ],
    months_abbr: names![
        ["Jan.", "Jan.", "Jan"],
        ["Feb.", "Feb.", "Feb"],
        ["März", "März", "Mär.", "Mär"],
        ["Apr.", "Apr.", "Apr"],
        ["Mai"],
        ["Juni", "Juni", "Jun.", "Jun"],
        ["Juli", "Juli", "Jul.", "Jul"],
        ["Aug.", "Aug.", "Aug"],
        ["Sept.", "Sept.", "Sep.", "Sep"],
        ["Okt.", "Okt.", "Okt"],
        ["Nov.", "Nov.", "Nov"],
        ["Dez.", "Dez.", "Dez"],
    ],
    days: names![
        ["Montag"],
        ["Dienstag"],
        ["Mittwoch"],
        ["Donnerstag"],
        ["Freitag"],
        ["Samstag", "Sonnabend"],
        ["Sonntag"],
    ],
    days_abbr: names![
        ["Mo.", "Mo"],
        ["Di.", "Di"],
        ["Mi.", "Mi"],
        ["Do.", "Do"],
        ["Fr.", "Fr"],
        ["Sa.", "Sa"],
        ["So.", "So"]
    ],
    periods: names![["AM"], ["PM"]],
};

pub static FR: Names = Names {
    months: names![
        ["janvier"],
        ["février"],
        ["mars"],
        ["avril"],
        ["mai"],
        ["juin"],
        ["juillet"],
        ["août"],
        ["septembre"],
        ["octobre"],
        ["novembre"],
        ["décembre"],
    ],
    months_abbr: names![
        ["janv.", "janv.", "janv"],
        ["févr.", "févr.", "févr"],
        ["mars"],
        ["avr.", "avr.", "avr"],
        ["mai"],
        ["juin"],
        ["juil.", "juil.", "juil"],
        ["août"],
        ["sept.", "sept.", "sept"],
        ["oct.", "oct.", "oct"],
        ["nov.", "nov.", "nov"],
        ["déc.", "déc.", "déc"],
    ],
    days: names![
        ["lundi"],
        ["mardi"],
        ["mercredi"],
        ["jeudi"],
        ["vendredi"],
        ["samedi"],
        ["dimanche"],
    ],
    days_abbr: names![
        ["lun.", "lun"],
        ["mar.", "mar"],
        ["mer.", "mer"],
        ["jeu.", "jeu"],
        ["ven.", "ven"],
        ["sam.", "sam"],
        ["dim.", "dim"]
    ],
    periods: names![["AM"], ["PM"]],
};

pub static ES: Names = Names {
    months: names![
        ["enero"],
        ["febrero"],
        ["marzo"],
        ["abril"],
        ["mayo"],
        ["junio"],
        ["julio"],
        ["agosto"],
        ["septiembre", "septiembre", "setiembre"],
        ["octubre"],
        ["noviembre"],
        ["diciembre"],
    ],
    months_abbr: names![
        ["ene"],
        ["feb"],
        ["mar"],
        ["abr"],
        ["may"],
        ["jun"],
        ["jul"],
        ["ago"],
        ["sept", "sept", "sep"],
        ["oct"],
        ["nov"],
        ["dic"],
    ],
    days: names![
        ["lunes"],
        ["martes"],
        ["miércoles"],
        ["jueves"],
        ["viernes"],
        ["sábado"],
        ["domingo"],
    ],
    days_abbr: names![
        ["lun"],
        ["mar"],
        ["mié"],
        ["jue"],
        ["vie"],
        ["sáb"],
        ["dom"]
    ],
    periods: names![["a. m.", "a.m."], ["p. m.", "p.m."]],
};

pub static IT: Names = Names {
    months: names![
        ["gennaio"],
        ["febbraio"],
        ["marzo"],
        ["aprile"],
        ["maggio"],
        ["giugno"],
        ["luglio"],
        ["agosto"],
        ["settembre"],
        ["ottobre"],
        ["novembre"],
        ["dicembre"],
    ],
    months_abbr: names![
        ["gen"],
        ["feb"],
        ["mar"],
        ["apr"],
        ["mag"],
        ["giu"],
        ["lug"],
        ["ago"],
        ["set"],
        ["ott"],
        ["nov"],
        ["dic"],
    ],
    days: names![
        ["lunedì"],
        ["martedì"],
        ["mercoledì"],
        ["giovedì"],
        ["venerdì"],
        ["sabato"],
        ["domenica"],
    ],
    days_abbr: names![
        ["lun"],
        ["mar"],
        ["mer"],
        ["gio"],
        ["ven"],
        ["sab"],
        ["dom"]
    ],
    periods: names![["AM"], ["PM"]],
};

pub static PT: Names = Names {
    months: names![
        ["janeiro"],
        ["fevereiro"],
        ["março"],
        ["abril"],
        ["maio"],
        ["junho"],
        ["julho"],
        ["agosto"],
        ["setembro"],
        ["outubro"],
        ["novembro"],
        ["dezembro"],
    ],
    months_abbr: names![
        ["jan.", "jan.", "jan"],
        ["fev.", "fev.", "fev"],
        ["mar.", "mar.", "mar"],
        ["abr.", "abr.", "abr"],
        ["mai.", "mai.", "mai"],
        ["jun.", "jun.", "jun"],
        ["jul.", "jul.", "jul"],
        ["ago.", "ago.", "ago"],
        ["set.", "set.", "set"],
        ["out.", "out.", "out"],
        ["nov.", "nov.", "nov"],
        ["dez.", "dez.", "dez"],
    ],
    days: names![
        ["segunda-feira"],
        ["terça-feira"],
        ["quarta-feira"],
        ["quinta-feira"],
        ["sexta-feira"],
        ["sábado"],
        ["domingo"],
    ],
    days_abbr: names![
        ["seg.", "seg.", "seg"],
        ["ter.", "ter.", "ter"],
        ["qua.", "qua.", "qua"],
        ["qui.", "qui.", "qui"],
        ["sex.", "sex.", "sex"],
        ["sáb.", "sáb.", "sáb"],
        ["dom.", "dom.", "dom"]
    ],
    periods: names![["AM"], ["PM"]],
};

/// Chinese: a month is its number and 月 (the numeral form wide,
/// the digit form abbreviated); weekdays 星期一 … 星期日.
pub static ZH: Names = Names {
    months: names![
        ["一月", "一月", "1月"],
        ["二月", "二月", "2月"],
        ["三月", "三月", "3月"],
        ["四月", "四月", "4月"],
        ["五月", "五月", "5月"],
        ["六月", "六月", "6月"],
        ["七月", "七月", "7月"],
        ["八月", "八月", "8月"],
        ["九月", "九月", "9月"],
        ["十月", "十月", "10月"],
        ["十一月", "十一月", "11月"],
        ["十二月", "十二月", "12月"],
    ],
    months_abbr: names![
        ["1月"],
        ["2月"],
        ["3月"],
        ["4月"],
        ["5月"],
        ["6月"],
        ["7月"],
        ["8月"],
        ["9月"],
        ["10月"],
        ["11月"],
        ["12月"],
    ],
    days: names![
        ["星期一"],
        ["星期二"],
        ["星期三"],
        ["星期四"],
        ["星期五"],
        ["星期六"],
        ["星期日", "星期天"],
    ],
    days_abbr: names![
        ["周一"],
        ["周二"],
        ["周三"],
        ["周四"],
        ["周五"],
        ["周六"],
        ["周日"]
    ],
    periods: names![["上午"], ["下午"]],
};

/// Japanese: a month is its number and 月, one form; weekdays
/// 月曜日 … 日曜日, abbreviated to the one character.
pub static JA: Names = Names {
    months: names![
        ["1月"],
        ["2月"],
        ["3月"],
        ["4月"],
        ["5月"],
        ["6月"],
        ["7月"],
        ["8月"],
        ["9月"],
        ["10月"],
        ["11月"],
        ["12月"],
    ],
    months_abbr: names![
        ["1月"],
        ["2月"],
        ["3月"],
        ["4月"],
        ["5月"],
        ["6月"],
        ["7月"],
        ["8月"],
        ["9月"],
        ["10月"],
        ["11月"],
        ["12月"],
    ],
    days: names![
        ["月曜日"],
        ["火曜日"],
        ["水曜日"],
        ["木曜日"],
        ["金曜日"],
        ["土曜日"],
        ["日曜日"],
    ],
    days_abbr: names![["月"], ["火"], ["水"], ["木"], ["金"], ["土"], ["日"]],
    periods: names![["午前"], ["午後"]],
};

/// Arabic: CLDR's `ar` month names (the set the Gulf and Egypt
/// write), with the Levantine names as further spellings, so
/// both parse; writing uses the default set.
pub static AR: Names = Names {
    months: names![
        ["يناير", "يناير", "كانون الثاني"],
        ["فبراير", "فبراير", "شباط"],
        ["مارس", "مارس", "آذار"],
        ["أبريل", "أبريل", "نيسان", "ابريل"],
        ["مايو", "مايو", "أيار"],
        ["يونيو", "يونيو", "حزيران"],
        ["يوليو", "يوليو", "تموز"],
        ["أغسطس", "أغسطس", "آب", "اغسطس"],
        ["سبتمبر", "سبتمبر", "أيلول"],
        ["أكتوبر", "أكتوبر", "تشرين الأول", "اكتوبر"],
        ["نوفمبر", "نوفمبر", "تشرين الثاني"],
        ["ديسمبر", "ديسمبر", "كانون الأول"],
    ],
    months_abbr: names![
        ["يناير"],
        ["فبراير"],
        ["مارس"],
        ["أبريل"],
        ["مايو"],
        ["يونيو"],
        ["يوليو"],
        ["أغسطس"],
        ["سبتمبر"],
        ["أكتوبر"],
        ["نوفمبر"],
        ["ديسمبر"],
    ],
    days: names![
        ["الاثنين"],
        ["الثلاثاء"],
        ["الأربعاء"],
        ["الخميس"],
        ["الجمعة"],
        ["السبت"],
        ["الأحد"],
    ],
    days_abbr: names![
        ["الاثنين"],
        ["الثلاثاء"],
        ["الأربعاء"],
        ["الخميس"],
        ["الجمعة"],
        ["السبت"],
        ["الأحد"]
    ],
    periods: names![["ص"], ["م"]],
};

/// Turkish: months one form, lowercase as CLDR has them; the
/// abbreviations capitalized as CLDR has those.
// CLDR's format forms are capitalized (Ocak, Salı): Turkish
// capitalizes month and weekday names inside a date, which is the
// only place strftime writes them. Reading is case-folded.
pub static TR: Names = Names {
    months: names![
        ["Ocak"],
        ["Şubat"],
        ["Mart"],
        ["Nisan"],
        ["Mayıs"],
        ["Haziran"],
        ["Temmuz"],
        ["Ağustos"],
        ["Eylül"],
        ["Ekim"],
        ["Kasım"],
        ["Aralık"],
    ],
    months_abbr: names![
        ["Oca"],
        ["Şub"],
        ["Mar"],
        ["Nis"],
        ["May"],
        ["Haz"],
        ["Tem"],
        ["Ağu"],
        ["Eyl"],
        ["Eki"],
        ["Kas"],
        ["Ara"],
    ],
    days: names![
        ["Pazartesi"],
        ["Salı"],
        ["Çarşamba"],
        ["Perşembe"],
        ["Cuma"],
        ["Cumartesi"],
        ["Pazar"],
    ],
    days_abbr: names![
        ["Pzt"],
        ["Sal"],
        ["Çar"],
        ["Per"],
        ["Cum"],
        ["Cmt"],
        ["Paz"]
    ],
    periods: names![["ÖÖ"], ["ÖS"]],
};

/// The table for a BCP 47 locale: the language decides (`ru-RU`,
/// `ru`, `ru_RU` alike). The locales shipped, by size of language:
/// en, zh, es, ar, pt, ru, ja, de, fr, tr, it.
pub fn names(locale: &str) -> Option<&'static Names> {
    // a region the CLDR tailors has its own names
    if let Some(r) = crate::regional::regional(locale) {
        return Some(r.names);
    }
    let lang = crate::language(locale);
    Some(match lang.as_str() {
        "en" | "" => &EN,
        "zh" => &ZH,
        "es" => &ES,
        "ar" => &AR,
        "pt" => &PT,
        "ru" => &RU,
        "ja" => &JA,
        "de" => &DE,
        "fr" => &FR,
        "tr" => &TR,
        "it" => &IT,
        _ => return None,
    })
}

/// Match one of `entries`' spellings at the start of `rest`, case
/// folded, the longest spelling first (so `Sept` beats `Sep`).
/// Returns the entry's index and the matched byte length in
/// `rest`.
pub fn match_name(rest: &str, entries: &[&[&str]]) -> Option<(usize, usize)> {
    let lower = rest.to_lowercase();
    let mut best: Option<(usize, usize, usize)> = None; // (entry, chars, bytes)
    for (k, spellings) in entries.iter().enumerate() {
        for spelling in spellings.iter() {
            let s = spelling.to_lowercase();
            if lower.starts_with(&s) {
                let chars = s.chars().count();
                if best.is_none_or(|(_, c, _)| chars > c) {
                    // Case folding keeps the char count for these
                    // tables, so the match's byte length in the
                    // original is that many chars in.
                    let bytes = rest
                        .char_indices()
                        .nth(chars)
                        .map_or(rest.len(), |(i, _)| i);
                    best = Some((k, chars, bytes));
                }
            }
        }
    }
    best.map(|(k, _, bytes)| (k, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_parse_and_the_longest_wins() {
        let ru = names("ru-RU").unwrap();
        assert_eq!(
            match_name("октября 1899", &ru.months),
            Some((9, "октября".len()))
        );
        assert_eq!(
            match_name("Октябрь", &ru.months),
            Some((9, "Октябрь".len()))
        );
        assert_eq!(match_name("мая", &ru.months), Some((4, "мая".len())));
        let en = names("en").unwrap();
        assert_eq!(match_name("Sept 3", &en.months_abbr), Some((8, 4)));
        assert_eq!(match_name("sep 3", &en.months_abbr), Some((8, 3)));
        assert!(names("xx").is_none());
        assert!(names("de_DE").is_some());
    }
}
